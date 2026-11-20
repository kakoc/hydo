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
4. Update the rows of `docs/USE-CASES.md` for each capability that the milestone builds or changes.

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
- Development builds carry no debug information (`[profile.dev]` in `Cargo.toml`), to keep
  `target/` small. A panic still names its file and line. For a debugger or file and line in a
  backtrace, build once with `CARGO_PROFILE_DEV_DEBUG=true` or `=line-tables-only`.
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
- The fingerprint epoch rule. `hydo_image::EPOCH` (now 5, since M317; 4 from M269, 3 from M234, 2 from M204, 1 from M155) names the
  generation of fingerprints. Raise it by one in the same commit as any change that moves a
  recorded fingerprint. Such changes are changes of an engine, the host controller, the agent pin
  or the guest kernels, or of a default that an image's run depends on. Add a section of
  `docs/REBASELINE.md` that names the new epoch. Runs of one epoch compare fingerprints. Runs of
  different epochs compare only verdicts. A default that changes with an epoch is keyed on the
  epoch in the image's record (`hydo_image::defaults`), so that an image built before keeps
  running as it did.
- Re-recording runs in batches, not per change (user, 2026-11-14). The recorded values stay
  strict, but a change that moves recorded values does not re-record the examples itself. It
  says which values it moves in its milestone and in open item 105. The lead re-records them in
  one pass overnight or before a release (`hydo-bench rebaseline`, the full-tier goldens, the
  KVM runners), and every value is re-recorded before a release is published. Quick checks
  during the day stay the pinned reproducers and the tests of the changed crates.

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

**Releases.** hydo 0.4.1 was published on 2026-11-19 from the release commit `14e09873`, in
these places:

- The GitHub release `v0.4.1`: both tarballs, their sources archives, and the Mac signatures.
- The Homebrew tap: `brew install kakoc/tap/hydo`.
- crates.io: the crates of `cargo install hydo`, and `hydo-assert`.
- PyPI and npm: `hydo-assert`. Go: the tag `sdk/go/v0.4.1`.
- The skill release `skill-v2.4.0` (the plugin `hydo`, command `/hydo:onboard`).

Since M332 the skill has hydo's version: its four version fields equal `hydo/VERSION` (0.4.1
now, `skill_check::` checks it). From 0.5.0 on the release `v<version>` carries the asset
`hydo-skill-<version>.tar.gz`, which both `build-release.sh` scripts make. 0.5.0 is the first
release with it. After it, the lead deletes the releases `skill-v1.0.0` to `skill-v2.4.0` and
their tags (docs/RELEASING.md, "Once, after 0.5.0 is published").

The release 0.4.1 is 0.4.0 with `hydo update` (M315), the five fixes of M316, and the skill
2.4.0. It came early, so that the manual upgrade from 0.4.0 is the last one (user, 2026-11-18).
Its fingerprint epoch is 4. It was cut from 0.4.0's release commit on the branch
`release-0.4.1`, which is merged into `main` (`docs/HISTORY.md` "The 0.4.1 release, published").
The rest of `main` after 0.4.0 waits for 0.5.0, that is Milestones 304 to 314 and 317 to 322,
and the fingerprint epoch 5. The release 0.4.0 was published on 2026-11-18 from `75db9ff9`,
with the skill release `skill-v2.3.3`. It adds Milestones 254 to 256 and 258 to 303 to 0.3.4, with the
fingerprint epoch 4. The files and prepared material of both releases are in
`hydo/dist/out/0.4.0/` and `hydo/dist/out/0.4.1/` of the main checkout. hydo 0.3.4 was
published on 2026-11-15 from `dee7da68`, with the skill release `skill-v2.3.1`.
The registries are published by `release.yml` on the tag. 0.3.4 adds Milestones 231, 233–253
and 257 to 0.3.3, as `docs/HISTORY.md` "The 0.3.4 release" says. The fingerprint epoch is 3. Both
releases ship M234's guest kernels with cgroups next to the old ones. 0.3.3 (`dad45fc9`) added
Milestones 228–230, 232, 239, and 240. A release with features raises the middle number, and the
last number is for small fixes only (user, 2026-11-15).
Plans keep `hydo_version: "0.3"` across releases: since M262 it means 0.3 or any newer 0.x, and
only an older hydo refuses a plan (`docs/RELEASING.md` step 1). The release index `hydo/dist/releases.txt` pins every published archive.
Since 0.2.0 the Linux release ships the KVM engine, and the Mac programs are signed and notarized.
`cargo install hydo` followed by `hydo setup` works on the Mac and, since M210, on Linux with KVM.
How to release: `docs/RELEASING.md` and `docs/PUBLISHING.md`. The files of each release and its
prepared material are in `hydo/dist/out/<version>/` of the main checkout. Their sha256 values are
in the release's section of `docs/HISTORY.md`.

**What 0.3.2 changed in the release** (`docs/HISTORY.md` "The 0.3.2 release"). The fingerprint
epoch stays 2.
The Linux release ships the KVM engine's SMP guest kernel `guest/bzImage-kvm-smp`, so
`--backend kvm --vcpus N` runs from it. Release step 6a ran on quick candidates and its fixes are
in. The released schemas (`hydo/schema/released/`) are the current ones again, a step that
0.2.0 to 0.3.1 skipped and docs/RELEASING.md step 1 now names. The release commit is `fbee09d4`.
Its five files and `SHA256SUMS` are also in `hydo/dist/out/0.3.2/` of the main checkout
(git-ignored), each platform built twice to the same bytes:

- `hydo-linux-0.3.2.tar.gz` `a6c1ef923843483b74be7f90e762be8e95499b4246f7b287ce4280847e160940`;
- `hydo-linux-0.3.2-sources.tar.gz` `58dbf9944d1f20f370ad7ed2addbf923651843d3571a2982942b89fe2329e5f7`;
- `hydo-mac-0.3.2.tar.gz`, signed and notarized,
  `2aec23c47241590d722e9de77e3a68c267045a0ed740c3450b757fe30505c03b`;
- `hydo-mac-0.3.2-sources.tar.gz` `bd5abc9010f871f9166ec39bbed7957ab6f626d9d3c0be16176ada20c21671ec`;
- `hydo-mac-0.3.2-signatures.tar.gz` `f1fa4a37d754d97edc7a1985eb7fa6cd92dd730c1dd50f6691ddaa85c1499527`.

The prepared material (release notes, the Homebrew formula, the release-index lines) is in
`hydo/dist/out/0.3.2/prepared/`. The KVM verification ran on GitHub's runners through a
one-off branch, now deleted, because the build machine cannot be reached. It passed on two
AMD EPYC models and on Intel Xeon 6975P-C hosts. On Intel a KVM fingerprint holds for one CPU
model only (open item 102).

**Registry publishing from GitHub Actions.** A tag `v<major>.<minor>.<patch>` starts
`.github/workflows/release.yml`. It publishes the crates, the PyPI and npm packages, and the Go tag
with each registry's trusted publishing, so no token is stored. 0.3.0 and 0.3.1 were published
this way. Other tags, such as the action's moving tag `v1`, do not start it. `packages.yml`
dry-runs the packages on every pull request that changes one. The details are in
`docs/PUBLISHING.md`, section "Publishing from GitHub Actions" (M186).

**Fingerprints.** The fingerprint epoch is 5 (M317; 4 from M269, 3 from M234, 2 from M204, 1
from M155). An image built at epoch 5 has the image services' launchers of M317, which mount the
system folders once per boot. Since M331 they also give each image service a `/tmp` of its own. On hvx an x86_64 image built at epoch 5 keeps time with its TSC
(M323, `guest.exact_tick`). The recorded values are still epoch 4's (open item 105). An image built at epoch 3 or later boots the Mac and KVM guest kernels
with cgroups. An image built
at epoch 4 carries no saved regression: `hydo build` writes them beside it, as
`<image>.regressions.json`, so saving or editing one leaves the image's bytes and the other
regressions' runs alone. An image built before keeps them inside and is replayed from them. An
image built at epoch 4 also boots with cgroup2 mounted on `/sys/fs/cgroup` (M283). M302
re-recorded every value of the list at epoch 4. The pinned reproducers are
`disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24` under `hydo run`: `a76d7fe857fdb37c`
(VIOLATION, order 11) on `explore-ob-stack-old-disk.img` (`dab0e76c…`) and `efbde7b57264be0f`
(OK) on `explore-ob-stack-fix-disk.img` (`d61cbf00…`), in `~/hydo/build/m204` on the bare-metal
machine. Their epoch-3 images (`b498b9c8…` and `c8fc9b42…`, in `~/hydo/build/m244`) give the same
two values on hvx. Their epoch-4 images (`728e344c…` and `b83e3d39…`, in `~/hydo/build/m302`)
give `e153f376096a072c` and `6a5c2fe208174af3` on hvx and `66da20bdbfafd6d2` and
`0bcce788f5ef3f4d` on KVM (AMD). Their epoch-1 images in `~/hydo/build/m155` still give `1dc3677fb94053cf` and
`ba0aa0612502a1df` under this hydo, because an image keeps the defaults of its own epoch
(`hydo_image::defaults`). `hydo/onboard/examples/stack/build-reproducers.sh` rebuilds both
images. The quickstart's Redis example gives `05f433ee427f8e3c` (OK) and `2174995ebfb4d2d6`
(`C=12 R=everysec`, `k:0` lost) on the Mac, and `7adecc09c07aaaa4` and `fd41aeb199057728` (`k:1`
lost) on KVM (AMD). Every recorded example value is in `hydo/onboard/recorded.json`, and
`hydo-bench rebaseline` checks or re-records them (`docs/REBASELINE.md` "How to re-baseline").

**The hvx engine.** The module (`hv/hvx/hvx.c`, ABI `hvx_abi.h`) boots Linux 6.6.35
deterministically, with guest RAM up to 8 GiB. Its clock counts VM exits, or the guest's retired
conditional branches with the PMU branch clock. Every module feature is opt-in; the engine turns on the
host-call port by default (M140), also with several vCPUs since M205. An image built at the
fingerprint epoch 5 boots with `tsc=reliable` and a virtual TSC step of 1 µs, so the guest's
clocks follow the virtual TSC exactly (M323). Since M261 a `cf=` power
cut comes at the first stop after the k-th flush, not at the workload's end. The run engine is the Rust
crate `hydo-hvx`, which runs inside `hydo` when the process can open `/dev/hvx`. Details:
`docs/HISTORY.md` Milestones 33, 66–69, 132, 137, 138, 156 and 179; the module's safety net is
`hv/hvx/SAFETY.md`.

**The Mac engine.** `hydo/crates/hydo-hvm` runs arm64 Linux 6.6.35 through Hypervisor.framework,
with two guest kernels: `Image-explore`, and `Image-explore-netns` for `network: bridge`. An image
built at the fingerprint epoch 3 or later boots their builds with cgroups, `Image-explore-cg` and
`Image-explore-netns-cg` (M234, `docs/design/guest-cgroups.md`). Its clock counts VM exits. The program is signed with `hv/hvm/hv.entitlements` before it runs.
An image's root file system is its initramfs, in guest RAM. With `guest.root: disk` (M284, a
format-1 addition, the default with `browser:`) the root is a read-only ext4 file system on the
disk, which a small initramfs mounts. `hydo build` makes it in a helper VM of the Mac engine.
From the fingerprint epoch 4 the disk's part of a fingerprint is a tree of 4 KiB block hashes
(M290, `hydo_hvm::disk`, also on KVM). The base's hashes are made once per process while the
guest runs, and a fingerprint hashes again only the blocks the run wrote. From the same epoch
the RAM's part is a tree of 4 KiB page hashes, made on half of the cores (M294,
`hydo_hvm::ramtree`). The KVM engine takes the hashes of the pages that the guest did not write
since its RAM base from the base. An older image keeps the byte-wise hash of every disk byte and
one chain of the word-wise mix over RAM. The details are in Milestones 48, 51, 99, 143, 147,
284, 290, and 294.

**The KVM engine.** `hydo/crates/hydo-kvm` runs the x86_64 images on stock Linux KVM, without the
module or root, with one vCPU and a patched guest kernel (`bzImage-kvm`). With `--vcpus N` it runs
2 to 8 vCPUs in turns of counted exits with the SMP kernel `bzImage-kvm-smp`
(`docs/design/kvm-vcpus.md`, M219). An image built at the fingerprint epoch 3 or later boots their
builds with cgroups, `bzImage-kvm-cg` and `bzImage-kvm-smp-cg` (M234; `hydo_kvm::kernel` picks by
the image's epoch), and `hydo explore --turns` and `--sweep turns` draw and search
those turns (M227). The vCPU is at the
x86-64-v2 level, or at x86-64-v3 for `guest.cpu: v3` on a host whose KVM has AVX2 (M189;
`hv/kvm/v3-check.sh`). It runs on Intel and on AMD (GitHub's ubuntu-24.04 runners, 2026-11-08).
Its fingerprints are its own. They differ between CPU vendors, because the guest's FXSAVE writes
other bytes into its RAM on AMD. On Intel they differed between CPU models only in DR6's host
bits, which leave the fingerprint of an image built at epoch 3 or later (M252). The AMD EPYC
7763, 9V45, and 9V74 agree, and the recorded KVM values are theirs since M244, and at epoch 4 since M302. The verdicts are
the same. Details:
`docs/design/kvm-engine.md`, Milestones 159, 161, 175 and 189, and open item 45 in
`docs/HISTORY.md` Milestone 187. Some guest loops make no VM exits and store to memory, for
example TigerBeetle's polling of io_uring. The full-state cycles prove such a loop by the whole
vCPU state and a dirty-page compare (M194; `hydo/explore/RESEARCH-kvm-exit-free.md`). With them
TigerBeetle's single replica gives one fingerprint per schedule on Intel and on three AMD EPYC
models. Since the fingerprint epoch 2 (M204) they are on by default. Every KVM fingerprint now
leaves out what KVM keeps of a finished event. `HYDO_KVM_SPIN_STATE=0` turns the cycles off. An
image built at epoch 1 runs as it did then (`hydo-kvm --image-epoch 1`).

**Loops without VM exits (M274).** The Mac and KVM engines deliver interrupts at exits, so a
loop without exits gets no timer interrupt. The spin observers resolve loops that repeat the
whole machine; on the Mac the observer also looks at the return address since M274, so
TigerBeetle boots there. `guest.branch_tick: true` (a format-1 addition) makes the KVM engine
count the guest's user-mode conditional branches with a host performance counter. After 2^26 of
them without an exit it stops the vCPU at the exact count. Trap-flag steps from the counter's
overflow reach the count, and the timer comes there as at an idle HLT. It needs an Intel host whose counters count a
guest; of GitHub's runners only the Xeon 6975P-C hosts do. The Mac engine refuses the key, and
hvx ignores it. The design and the measurements are in `docs/design/exit-free-loops.md`.

**The guest's clock on hvx (M319 and M323, tentative numbers).** Without a word on its command
line, hvx's guest kernel keeps time with `refined-jiffies`, which counts a tick as 999,848 ns.
Its periodic tick sets each next tick 1 ms after the one before. After t seconds of uptime a tick
is due 1 ms + 0.152 ms × t ahead on the virtual TSC. A busy guest late in a long run then needs
that much virtual time per tick. Its clock nearly stops, as in the ClickHouse runs that ended
`TIMEOUT workload` at 900 s (open item 157). Since M323 `guest.exact_tick` (a format-1 addition)
is on by default for an x86_64 image built at the fingerprint epoch 5. The hvx engine then boots
the guest with `tsc=reliable` and a virtual TSC that moves 1 µs per step. The guest's clocks are the
virtual TSC, 1.000003 s per second of it at every uptime. A clock read costs 2 µs of guest time,
and a program sees steps of 2 µs.

`tsc=reliable` with the old step of 41 µs made a read cost 82 µs, and ClickHouse then missed its
readiness deadline. M319's `clocksource=jiffies` stays for images built with its key: exact
ticks, but a busy guest's clock at 0.44 of the virtual TSC and steps of 1 ms. An image built
before epoch 5 boots as before. The Mac and KVM guest kernels read one counter for the clock and
the timer, so they have no gap. The reference is the section "The guest's timer tick on hvx" of
MANIFEST.md; the comparison of the four ways is in `docs/HISTORY.md` Milestone 323.

**The cgroup2 mount (M283, tentative number).** From the fingerprint epoch 4 the host controller
mounts the cgroup2 file system on `/sys/fs/cgroup` at the start of every boot, before the S0
snapshot, with the agent's existing `mount` primitive. So the agent and its pin stay, and an
image's bytes do not change with it. A recovery boot and a reboot start from S0 and have it too.
`guest.cgroup2: false` (a format-1 addition) turns it off, and an image built before epoch 4
mounts nothing, as before (`hydo_image::defaults`, `cgroup2`). A program that reads
`/proc/self/cgroup` at start (Seastar, Dragonfly) finds its `0::/` line, so the Dragonfly and
Redpanda examples no longer mount cgroup2 in `setup:`. A pause of threads uses the mount and
mounts nothing more. An image service's root of an image built before epoch 5 gets `/sys` by a
bind that is not recursive: its programs see the `0::/` line but no cgroup2 files. From epoch 5
the bind is recursive, so they see the files too (M338). hvx, the Mac engine and KVM (four Intel
runners) ran it; a second KVM run of the one-off workflow `tmp-cgroup2-kvm` is open item 144. The reference is MANIFEST.md "The cgroup2
mount"; the design is the last section of `docs/design/guest-cgroups.md`.

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

**Onboarding and SDKs.** `hydo onboard` starts a repository's `hydo/` folder and installs the
skill (M229). The skill `onboard` in the Claude Code plugin `hydo` (`plugin/skills/onboard/`;
`/plugin marketplace add kakoc/hydo`, `/plugin install hydo@hydo`, then `/hydo:onboard`) holds
the process; every hydo fact is in the guide `hydo/guide/`, printed by `hydo guide <topic>`.
docs/ONBOARD.md is the user's page. `docs/ONBOARDING-PLAYBOOK.md` is the process for an onboarding
of a real project, from the choice of a target to the upstream report (M281). `hydo guide
strategies` lists the checks and faults that found bugs, each with its evidence, so that an
onboarding chooses them from what worked (M293). M330 (tentative number) added four entries
from a user's second onboarding round, which found two bugs where the first round found none.
They are production's topology, the features behind a flag, the checks of how each unknown
outcome ends, and pauses past a failure detector. The skill copies
production's shape from the deployment manifests and marks promises that no owner confirmed. It
reads the build's RAM warnings before the first run and asks for a second round before it
reports no bugs. Its version stays until the lead sets it. The assertion SDKs are in `sdk/`
(Rust is the reference), with the shared cases in `sdk/cases/`. `sdk/README.md` is for SDK
maintainers: the shared cases, and the Go module with its frozen copy in `hydo/stack` (section
"The Go module and the frozen copy in `hydo/stack`"). Every code example on the package pages is a
test in its language (M188). The examples are in `hydo/onboard/examples/`;
`hydo/onboard/MANIFEST.md` is the project format's reference.

**A user's onboarding with 0.4.0 on the Mac (M316, tentative number).** Five fixes, released in
0.4.1, each its own commit; none moves a recorded value, and the epoch stays 4:
- `hydo.yaml` reads YAML anchors, aliases and `<<` merge keys, and drops top-level `x-` keys
  (MANIFEST.md "YAML subset").
- An image's FIFOs are FIFOs in the service's root; device nodes stay left out. No recorded
  image has a FIFO.
- `emulators:` works with `network: bridge`: the service gets `addr:` (a format-1 addition, or
  the first free 10.89.1.x) and listens on 0.0.0.0, and a `root.text` `/etc/hosts` gets the
  names.
- A service that its environment stopped in a recovery boot keeps its verdict. When its log
  has `Permission denied` and no damaged file, the diagnosis says that the run shows no lost data.
- MANIFEST.md no longer says that `hydo compose` passes `--no-addr` on a Mac. The explorer's
  memory warning on the Mac and KVM no longer counts the image beside the guest's RAM.

The user's tmpfs that was missing in a recovery boot was not reproduced then. Milestone 317 fixes
the race that hid it, at the fingerprint epoch 5.

**An image service's system folders mounted once (M317, tentative number; fingerprint epoch
5).** An image service's launcher and each of its `hydo-oci` helpers bound `/proc`, `/sys`,
`/dev`, `/tmp` and `/data` into the service's root on every start, outside the lock of its
`mounts:`. A readiness check that started with the service could bind `/dev` a second time over
the service's `/dev/shm` tmpfs, as in the user's recovery boot (open item 162). From the
fingerprint epoch 5 the launcher and the helpers bind the shared tree, the system folders and the
`mounts:` in one block per boot under that lock, and `/proc/mounts` decides each
(`hydo_image::defaults`, `oci_mounts_once`). An image built before keeps its launcher and its
values. On hvx a project of seven PostgreSQL image services showed the race before the fix and
not after it. Every image built at epoch 5 has new bytes, and the values wait for the pass before
0.5.0 (open item 105). The details are in `docs/HISTORY.md` Milestone 317.

**One command updates everything (M315, tentative number).** `hydo update [--check] [--yes]
[--version V]` takes the newest `v<x>.<y>.<z>` release of the release list and updates hydo the
way its own program's path says it was installed. A Homebrew keg gets `brew upgrade
kakoc/tap/hydo`, a cargo install `cargo install hydo --locked --version V --root <its root>` and
the new `hydo setup`. An `install.sh` tree gets only this platform's archive and `SHA256SUMS`,
checked by sha256, and the new `install.sh --prefix <tree>`, with `--no-module` on Linux unless
hvx is loaded. A version of the install cache gets the new hydo's `setup --archive` and `--get`.
A checkout's build changes nothing and gets the commands. Then the skill is updated in four
kinds of places:

- every copy that `hydo skill update` knows;
- the `skills/hydo` of each Claude Code configuration folder (`CLAUDE_CONFIG_DIR`, `~/.claude`,
  any `~/.claude*` with `plugins/` or `settings.json`);
- every copy in the registry `~/.hydo/skill-copies.txt`, which `hydo skill install`, `hydo skill
  update` and `hydo onboard` write (not for a copy in the temporary directory);
- the marketplace install `hydo@hydo` of each folder: `claude plugin marketplace update hydo`,
  then `claude plugin update hydo@hydo --scope <scope>`, with `CLAUDE_CONFIG_DIR` set.

 Only the hydo
plugin's entries and the skill's version are read in those folders. The plan names each part
with its versions and the "Before you upgrade" notes. `--check` exits 0 or 3, and without
`--yes` a non-terminal gets the plan and the exit status 3. `hydo doctor` names a newer release
once a day (`HYDO_UPDATE_CHECK=0` turns it off). The reference is `hydo guide update`; the skill
2.4.0 runs `hydo update --check` in its step 0. Since M322 the release list is read once per
command, kept for 10 minutes in `~/.hydo/update-check.json` (a day for `hydo doctor`), and
asked with `GH_TOKEN`, `GITHUB_TOKEN` or `gh auth token` when one is there. The token goes to
`https://api.github.com` only. At GitHub's limit for requests without a login, `hydo update` and
`hydo skill update` say so in one line with the reset time. They exit 4 (could not check), and
the doctor stays quiet until the reset. Since M332 the skill comes from hydo's own release: `hydo
update` takes the skill of the release it installs, `hydo skill update` that of the newest
release, both from `<release URL>/v<version>/hydo-skill-<version>.tar.gz`, checked by the
release's `SHA256SUMS`. A copy or a plugin install of the old line 1.0.0 to 2.4.0 counts as older
than any hydo version (`hydo_agents::release::skill_lt`). hydo 0.4.1 reads only `skill-v`
releases, so a user of 0.4.1 runs `hydo update` twice to reach 0.5.0's skill.

**Rules nobody wrote (M236, first version).** The skill proposes a project's business rules from
its own sources instead of a list per domain, and hydo validates them before the owner sees
them. `hydo rules scan [DIR]` lists the evidence with file and line. `hydo rules check
<candidates.yaml>` adds every candidate to the image's plan as a final check, without a rebuild.
It runs the workload without faults on three composition seeds and reports each rule as
`holds`, `vacuous`, `fails-clean`, `unevaluated` or `not-run`. `--mutate` shows whether a rule
catches a planted change, and `--write-checks DIR` writes the rules that hold as checks. The
reference is `hydo guide rules`, the design `docs/design/rule-suggestion.md`, the skill's step
`references/proposing-rules.md`. On Blnk, Lago, and listmonk (which hydo had never seen), 23 of
25 proposed rules held on clean runs. Two failed without faults: a wrong Blnk rule, fixed in two
steps, and a listmonk inconsistency (a blocklisting by hard bounces leaves the subscriptions
active). On the examples' saved regressions the proposed rules caught six findings. Two are
Blnk's (the overdraft bug, the lost commits) and four are Lago's (lago-api 6901, the double
invoice, the skipped billing day, the lost in-advance fees). Blnk's atomic-bulk bug was caught
only by a rule written after the miss. The proof files are in `docs/design/rule-suggestion/`.

**Compose services built locally (M214).** For a service with `build:`, `hydo compose` builds
the image with Docker (`docker`, else `podman`, or `--docker CLI`) for this machine's guest, with
Compose's build settings. It reads the image's settings from the saved file and pins the file's
digest in the lock file (`local_build: true`). fetch-images.sh builds a missing file with
`hydo-fetch-image build` and stops at a digest other than the pin until `HYDO_ACCEPT_REBUILD=1`.
`--no-build` leaves the builds to the script. Reference: `hydo guide workflow`.

**Images in private registries (M320, tentative number).** `hydo compose`, `hydo k8s` and
`hydo-fetch-image` take the credential that `docker login` saved, found as Docker finds it in
`$DOCKER_CONFIG/config.json` (default `~/.docker/config.json`, `hydo_compose::creds`). The order
is a `credHelpers` helper for the registry, then the `credsStore` helper, then an `auths` entry
(a password or an identity token). The registry gets it in its token flow or as basic
authentication. No secret is printed or written. A 401 names the registry and where hydo
looked, and suggests `docker login <host>` or `--from-docker`. With `--from-docker`, an image
whose registry refuses or does not answer is saved from the local Docker (`docker save
--platform`) and pinned in the lock file (`from_docker: true`). fetch-images.sh saves a missing
file again with `hydo-fetch-image docker` and stops at another digest until
`HYDO_ACCEPT_REBUILD=1`. A registry on the loopback is reached over plain HTTP, as Docker does.
The proof is a registry in the test process and Docker's `registry:2` with htpasswd, never a
real registry. Reference: `hydo guide workflow`.

**Three gaps of a user's onboarding with 0.4.0 (M324, tentative number).** Each fix is its own
commit; none moves a recorded value:
- `hydo compose` writes `root.share_images: true` for services of one image, with a comment of
  what it does (commented out from M324 to M331, open item 147).
  `hydo build` estimates each image's unpacked size from its layers, also with `--check`. It
  warns when the roots exceed half of `guest.ram_mib` on the Mac. On hvx it warns when the root
  and the data disk exceed 4096 MiB (MANIFEST.md, "Limits of v1").
- `report.md` and `hydo report` print, for each problem, the `hydo run` command that makes its
  run directory, `runs/seed-<n>` of the exploration's folder.
  `hydo inspect diagnose <explore dir> --problem N` reads that folder, the explorer's replay, or
  the exploration's own transcripts, and `--run` makes the folder (`hydo guide inspect`).
- The clock audit leaves out test code inside source files. It ranks first the code of the
  Cargo and Go programs that the manifest's services run. Its schedules name the one service
  that runs a place (`hydo guide faults`).

**Small fixes from a user's second onboarding round (M328, tentative number).** Each fix is its
own commit; none moves a recorded value:
- `hydo explore` exits with status 5 when it stops before it runs a schedule. Such a stop is a
  refusal for memory, a missing image, options that do not go together, or a failed engine.
  With `--json` it prints one JSON line with the reason (`hydo guide workflow`).
- `hydo inspect stop --all` stops every session of the user. A refusal of `hydo run` or
  `hydo explore` for memory names each session with its memory and age. A session ends after
  300 s without a call (900 before). `hydo inspect start` gives the id also at `.id`.
- The second one-shot `hydo inspect` call on one run folder within 10 minutes prints a hint to
  start a session.
- A failure without a power cut and with the safe settings has a class named for its fault
  families (`process`, `network+clock`, `no-fault`), not `durable-defaults`. The explorer still
  shrinks one failure per signature among them, as before.
- `hydo save`'s note on `W=@<key> (unconfirmed)` says what it means and how to confirm it.
- A boot whose kernel could not unpack the initramfs ends with its own ERROR, with the root's
  size and `guest.ram_mib`.
- A run that uses up the engine's exit budget names the check or the workload steps that ran.
  An `eventually_` check that failed 3 tries or more gives its violation instead (`hydo guide
  checks`, "The exit budget").
**The s3 emulator and the shell assertion SDK (M329, tentative number).** Three gaps of a
user's onboarding with 0.4.1, each fix its own commit; none moves a recorded value:
- An `http` proxy keeps the service's `Content-Length` and forwards whole request bodies (up to
  512 MiB). Before, a `HEAD` through the s3 proxy had no size, so rclone deleted each object it
  had uploaded and `object_store` failed; bodies were cut to 1 MiB (open item 162).
- rclone 1.76.0 answers `If-None-Match: *` and `If-Match` as S3 does (412). The s3 proxy's
  routes `put_if_absent` and `put_if_match` (the route key `if_header:`, and `errors:` on an
  `http` proxy's route, format-1 additions) let `H=s3.put_if_absent@3-6:status=412` make a lost
  race, and `--protocol-faults` draws 412 there.
- Both releases ship the shell assertion SDK as `hydo/sdk/hydo_assert.sh`, and `hydo guide
  hydo_assert.sh` prints it; `hydo guide sdks` puts it into an image with `root.files`. Its
  records reach the property table of `result.json` (a Mac engine run).
**Each image service's own `/tmp` (M331, tentative number; fingerprint epoch 5).** An image
service of an image built at epoch 5 has a `/tmp` of its own, as a container has. It is the
guest's folder `/tmp/oci/<service>` (`hydo_image::defaults`, `oci_own_tmp`). The guest's
`/tmp/hydo` is bound at its `/tmp/hydo` (the run files), and the guest's `/tmp` at `/.hydo/tmp`. Before, every
image service bound the guest's own `/tmp`, so services of one image met each other's lock files
there, with `root.share_images` and without it (TiKV's address lock, open item 147). `hydo
compose` writes `root.share_images: true` again for services of one image. On the Mac, with three
services of one Dragonfly image, the guest's root took 126 MiB of guest RAM with it and 362 MiB
without. The reference is MANIFEST.md "Services from a Docker/OCI image"; the details are in
`docs/HISTORY.md` Milestone 331.

**Hard links, the root's room, `/sys` and the pause of threads (M338, tentative number;
fingerprint epoch 5).** Four fixes, each its own commit and each keyed on the image's epoch in
`hydo_image::defaults`:
- An arm64 root keeps an image's hard links as links (`cpio_hard_links`). The busybox image's
  initramfs fell from 259 MB to 6.4 MB, and its root from 446 MiB of guest RAM to 14 MiB.
- `hydo build` and `--check` count the whole root against the Mac's guest RAM: the trees, hydo's
  own files, and the compressed root. The kernel holds the compressed root while it unpacks it
  into a tmpfs of half the RAM left (`oci::initramfs_room`). PostgreSQL 16.4 in 1,024 MiB now warns and names
  1,280 MiB; the build also measures the staged root.
- An image service binds `/sys` recursively, so it finds the cgroup2 files (`oci_sys_rbind`).
- A timed pause of threads takes the workload's largest session and leaves out a thread that
  ended before its move (`pause_largest_session`), as a reaction's freeze does.

The details are in `docs/HISTORY.md` Milestone 338.

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

**Blnk: business rules on a ledger with a queue (M224).** On Blnk's `main` (`67d88333`), seven
domain rules over Blnk's tables ran beside the generic rule over the client's acknowledged
operations. The runs used the Mac engine and hvx. Each kind found what the other did not. The overdraft
rule found a new Blnk bug: draws that the worker coalesces, or that the queue recovery applies,
lose their `overdraft_limit`. The bulk rule and the generic rule found another: an atomic bulk
keeps its first transfer when a later one fails before it is queued. Only the generic rule saw
acknowledged commits of holds lost with Redis's defaults. Both bugs have tested patches. The
example is `hydo/onboard/examples/blnk` (RESULTS.md).

**Valkey with replicas (M232).** Valkey 9.1.3, a primary and two replicas with `appendfsync
always`, ran 237 explored schedules on the Mac engine. Acknowledged writes survived every
fault but a disk that drops acknowledged writes. One new candidate is not reported upstream. The
primary times the wait before a full sync with the wall clock. So a backward clock step delays
the sync by the step's size. The same
wall-clock lag breaks `min-replicas-to-write` (native only). hydo also found the known issue
5354, a hash's expiry lost on AOF replay. The example is `hydo/onboard/examples/valkey`
(RESULTS.md, `native/`).

**River: a job queue's promises (M233).** River v0.51.2 runs in a small Go program with three
workers on PostgreSQL 16 in `hydo/onboard/examples/river` (RESULTS.md), started with `hydo
onboard` from its Compose file. A messages check and seven rules over the tables state River's
promises. Examples are no lost job, no job from a rolled-back transaction, no rerun after
`JobCompleteTx`, and one leader at a time. One rule broke by River's code. After a crash of
PostgreSQL two clients act as leader for seconds and insert one periodic run twice, because
`river_leader` is unlogged. The regression `K=postgres@6+1 W=44c763a6,p3,q2,r400` fails on
every hvx run, and `native/` reproduces it with Docker. No upstream report names it, and nothing
was posted. A forward clock step longer than River's 15 s lease gives two leaders too, with any
table: a clock effect. The runs used hvx only.

**The promises of Hatchet, a task queue (M250).** Hatchet v0.120.5 runs with two engines, the API
and three workers of a small Go program on PostgreSQL 16, which is also Hatchet's message queue,
in `hydo/onboard/examples/hatchet` (RESULTS.md). It was started with `hydo onboard` from its
Compose file. A messages check and ten rules state Hatchet's promises. Examples are no lost
acknowledged run, one run per cron tick and per scheduled run, concurrency and rate limits, DAG
order, and cancellations that hold. One new candidate breaks the first rule. Hatchet's PostgreSQL
queue sets a message `ASSIGNED` when an engine reads it and never gives it back. After a crash of
that engine, acknowledged scheduled runs and triggers never run, and finished runs run again. The
regression `K=hatchet_engine_1@6+3` fails on every hvx run, and `native/` reproduces the loss with
Docker. No upstream report names it, and nothing was posted. The runs used hvx only.

**Saleor: business rules on an e-commerce platform (M251).** Saleor 3.23.49, its API, one
Celery worker with beat, PostgreSQL and Redis, runs in `hydo/onboard/examples/saleor`
(RESULTS.md), imported with `hydo compose`. Customers pay through a payment app (a fake that
Saleor calls synchronously) and with Saleor's own gift card gateway; staff refund orders. The
rule suggestion read none of Saleor's 25 `models.py` files, so 14 of the 17 rules came from
reading the code; 16 held on a clean run. The blind exploration found two new candidates, and
`native/` reproduces both with Docker. Both are reported upstream (2026-11-15):
- A refund request that `transactionRequestAction` acknowledged is lost with its Celery task
  when the worker dies or the broker loses it. Saleor never sends it again or marks it failed.
  The regression `H=payments.refund@1-12:delay=6000 K=worker@6+1` breaks `refund_requests_end`
  on every hvx run. Reported as saleor/saleor#20022.
- A gift card charge that stops between its balance commit and its `CHARGE_SUCCESS` event leaves
  the card short and the order unpaid, and nothing can charge it again. The regression is a
  power cut at a disk flush (`cf=271` since M268) after a clock step. Reported as
  saleor/saleor#20023.

Two known bugs reproduced natively: overlapping refunds refund more than a payment charged
(saleor/saleor#19645), and a single-use code is spent by a completion that made no order
(#19919). The runs used hvx only.

**Software updates of embedded devices (M259).** RAUC 1.15.2 and SWUpdate 2026.05.1 update one
device model in `hydo/onboard/examples/rauc` and `swupdate` (RESULTS.md each). The device has two
root file system slots (loop devices on the data disk), U-Boot's redundant environment through
libubootenv 0.3.8, and the update tool's partition. Its boot is a script: U-Boot's boot script
and a verified boot that hashes the whole slot. A power cut's recovery boot and the agent's
reboots run it. Power cuts at every point of an install and a reboot found no RAUC bug. They
found one SWUpdate candidate, not reported upstream and not posted:
- SWUpdate's raw handler never flushes a written image, and SWUpdate then switches U-Boot's
  `boot_slot`. A power cut between a successful install and the reboot boots the slot's old
  contents: `disk=cut | run C=4` boots the factory's v1 instead of v3, older than the running v2.
- It needs slots whose cache the environment's flush does not reach, as with the environment on
  SPI NOR. With one eMMC for both (`--set dio=1`) the schedule passes. RAUC passes it.

The section "What hydo lacked" of the RAUC example lists what devices need from hydo (open item
125).

**Reboots and added disks (M263, tentative number).** Two format-1 additions for devices
(`docs/design/devices.md`; MANIFEST.md "Reboots" and "Added disks"):
- `guest.reboot: true`: a guest program asks for a reboot with the console line `HYDO-REBOOT`.
  hydo shuts the guest down cleanly with SIGTERM, SIGKILL 2 s later, `sync`, and a disk flush.
  The engine's new command `REBOOT` then starts a new boot from S0 with the disks' bytes. The next
  conversation (`reboot <n>`) carries the clock and `$HYDO_ACKED` and `$HYDO_OUTCOMES`, starts
  every service and goes on with the next workload step; each step is its own call. A reboot in a
  recovery boot gives a new recovery boot. Built on hvx (no module change), the Mac, and KVM.
- `guest.disks`: up to 4 disks beside the main one (`/dev/vdb` and on), each with a write cache of
  its own (`cache: volatile | none`); a power cut drops every volatile cache. The Mac and KVM
  engines; the hvx engine refuses them before any VM (open item 128).

Without the keys nothing changes: the pinned reproducers gave their values on hvx. The SWUpdate
example's `hydo-devices.yaml` puts the slots on an eMMC and the environment on SPI NOR flash. With
a real reboot and those disks, M259's SWUpdate candidate holds: `disk=cut | run C=4`
boots v1 on the Mac engine and on KVM, and with the loop model on hvx. RAUC held.

**Mender in standalone mode (M272, tentative number).** Mender 5.2.0's `mender-update` updates a
device in `hydo/onboard/examples/mender` (RESULTS.md). The device has A/B root partitions,
U-Boot's redundant environment and Mender's data partition on one eMMC, and an emulation of
Mender's U-Boot integration. The operator runs 8 standalone deployments with real reboots. Every
Mender command runs in a chroot on the booted slot. The Mac engine ran it with the eMMC as an
added disk, and hvx with the eMMC in a file (`hydo.yaml`, M276). Two new candidates were
found. Neither is reported upstream or posted:
- Mender syncs an Artifact's state scripts before the scripts' `version` file leaves a C++ stream
  buffer. After a power cut between an install and the next writeback, the file is empty. Then the
  healthy update can be neither committed nor rolled back, and Mender names its artifact
  `<name>_INCONSISTENT` (`disk=cut | run C=4`). Artifacts without state scripts pass.
- After a power cut or a kill inside `mender-update commit`, the next commit rolls the update
  back, prints `Rolled back.` and exits 0. When U-Boot had already made the update permanent, the
  rollback undoes it (`disk=cut | run A=mender.commit_started@3+7:cut` on the Mac; on hvx a cut
  69 to 87 ms after the event).

mender-artifact 4.4.2 writes an Artifact's state scripts in a random order; the build sorts them.

**Dragonfly with replicas (M278, tentative number).** Dragonfly 2.1.0 runs as one primary and two
replicas from its official image in `hydo/onboard/examples/dragonfly` (RESULTS.md), imported with
`hydo compose`, on hvx only. It needs `guest.cpu: v3`, because Dragonfly's Lua interpreter is
built for Sandy Bridge, a known requirement (issue #163). One new candidate is not reported
upstream or posted. Dragonfly loads the snapshot whose files have the latest modification time.
So after a backward step of the wall clock, a restart loads a snapshot from before the step. This
happens also after a clean `SHUTDOWN` that saves first, and every write since is lost. hydo found
it blind, and the shrinker reduced it to `T=8:-18` and the composition. A power cut loses
snapshots that `SAVE` acknowledged (known, #8646). With `--set takeover=yes`, `REPLTAKEOVER` kept
its promise in 80 schedules of kills, pauses, and partitions.

**SurrealDB on SurrealKV (M277, tentative number).** SurrealDB 4.0.1, whose only disk engine is
SurrealKV 1.2.0, runs as one server in `hydo/onboard/examples/surrealdb` (RESULTS.md), started
with `hydo onboard` from a Compose file. Eight drivers and nine checks state its promises:
list-append transactions at snapshot isolation, registers, a bank, large values, an `ASYNC`
event, a `UNIQUE` index, a graph, the changefeed, and live queries. One new candidate is in
SurrealKV. When a damaged value of the value log belongs to the WAL's newest large record, the
next open drops every later acknowledged commit and starts with a warning. With one more large
record after it, the open refuses. The blind explorations found it 7 times, with `G=` on the
value log. `native/` reproduces it with Docker; nothing was posted. Two known bugs were reached:
surrealdb#7606 and the open pull request #7672. `upgrade/` upgrades 3.3.5 on RocksDB to 4.0.1 in
the middle of a run, and every check held. The runs used hvx and the Mac engine.

**Redpanda (M279, tentative number).** Three nodes of Redpanda v26.2.7 run in one hvx guest in
`hydo/onboard/examples/redpanda` (RESULTS.md), each with one core, 1 GiB, and its own file system.
A Go client on franz-go produces idempotently, writes transactions and consumes as a group; one
`type: messages` check and the invariant `tx.no_hanging` judge it. Seastar needs cgroup2 mounted
once in the guest, which hydo does at boot since M283. Strict power losses of one node and
whole-guest cuts lost no acknowledged record, whether write caching was on or off. Four new
candidates; the third and the fourth are reported as redpanda-data/community#395 and #396:
- A damaged `tx.snapshot` of one partition stops its node at every start (an assertion in
  `persisted_stm.cc`), although a missing one is rebuilt from the log
  (`G=n1@1+2:zero,len=4096,file=tx.snapshot,seed=626bd178`; Docker too).
- A transaction whose coordinator record is still `empty` is never aborted. `do_try_abort` asks
  for a state change that the coordinator's state machine forbids from `empty`. So the
  partition's last stable offset stops for good. Two blind schedules reached it on hvx: one with
  four coordinator failovers, and one with a whole-guest power cut.
- A committed transaction is visible on one partition and not on the other. The client's commit
  succeeds. A second producer's InitProducerId with the same transactional id aborts the open
  transaction without an epoch bump (`do_init_tm_tx`, `do_abort_tm_tx`). The first producer's
  next AddPartitionsToTxn then starts a new transaction with only its partition, and its commit
  commits that one. Apache Kafka fences the first producer there. Two schedules reach it on hvx:
  `K=nodes.primary@66+1 P=n2@42+11`, and one kill of any node during a transaction,
  `A=rp.txn_add@20+3:kill-n1`. `native/half-commit.sh` reproduces it under Docker on 10 of 10
  tries.
- After a change of a partition's leader, a transactional produce answer names an earlier
  transaction's offset. The cache of a producer's requests matches by sequence range without the
  epoch, and only the leader clears it at a transaction's first batch (`requests::try_emplace`,
  `do_transactional_replicate`). A retry then gets another record's offset, and a new request
  gets one without being written, so a commit without error loses it. On KVM
  `A=rp.txn_add@20:kill-n3` gave the wrong offset on three hosts. `native/stale-offset.sh`
  gives both under Docker on 20 of 20 rounds with a leader move, and 0 of 20 without.

**Meilisearch's task queue (M295, tentative number).** Meilisearch v1.57.0 runs as one server
from its image in `hydo/onboard/examples/meilisearch` (RESULTS.md), started with `hydo onboard`
from a Compose file, with its data on a file system of its own. A Go client enqueues additions,
updates, deletions, settings, index swaps, edits by function, dumps and snapshots, and six checks
judge the queue's promises. No acknowledged task left the queue, and each index's tasks ran in
order. Three new candidates, found on the Mac engine and not posted:
- An index swap runs twice after a crash. The swap commits the swapped mapping in one LMDB
  transaction and the task's status in a later one, so the restart swaps the indexes back
  (`disk=none | run A=meili.add_acked@20+40:cut`). A SIGKILL in Docker does it in 4 of 48
  rounds (`native/swap-twice.sh`); hydo's kill reactions never came inside the window.
- A power cut empties the payload of an enqueued document addition: the file is renamed into
  place without an `fsync`. The task then succeeds with no document indexed, or fails its whole
  batch with `No such file or directory`.
- A succeeded dump is an empty file after a power cut: it is never synced.

**TiKV (M287, tentative number).** TiKV and PD v8.5.8 run as three PD members and three stores
in one hvx guest in `hydo/onboard/examples/tikv` (RESULTS.md), imported with `hydo compose`. The
stores use API version 2, so that RawKV and TxnKV share the cluster. A Go client on client-go v2
records four things:
- a list-append history, checked for strong snapshot isolation, also with async commit;
- raw registers with compare-and-swap, checked for linearizability;
- bank transfers;
- acknowledged writes of new keys.

It also asks PD to move, split, and merge regions. Every promise held under kills, pauses,
partitions, power cuts, one store's power loss and corrupted files, and clock steps. One clock
effect is known as pingcap/tidb#44822 and accepted there. After the wall clock steps back, PD's
timestamps keep their physical part (documented), and TiKV judges a lock's time to live by it
(`check_txn_status.rs`). So the locks of a client that crashed inside its commit block every
writer of their keys for the length of the step. `disk=none | run T=20:-3600` breaks the
`writable` check on every run. Without the step the locks go 2.18 s after the heal. The
exploration also reached a known PD bug blind (tikv/pd#11596, fixed on PD's master only). A PD
member that restarted shortly before a power cut stops at every later start with `leveldb:
manifest corrupted`. Its old goleveldb syncs `CURRENT` but not the new MANIFEST. The example
needed `root.share_images: true`, addresses between TiKV and PD, and 7,680 MiB of guest RAM (open
item 147).
**Restate's durable execution (M303, tentative number).** Restate v1.8.1-hotfix.2 runs as one
server from its image in `hydo/onboard/examples/restate` (RESULTS.md), imported with `hydo compose`,
with its data on a file system of its own. The deployment is the example's own service on
Restate's Rust SDK 0.13.0, one static musl program. Each journaled step appends a synced line to an
effects file, so the rules see a step that runs again. A messages check and four rules judge
calls and sends with idempotency keys, delayed sends, durable sleeps, adds to virtual objects,
awakeables, and workflow promises. They held under kills, pauses, network faults, and power cuts at
steps, at events, and at 49 disk flushes on hvx. One new candidate, found blind on the Mac engine,
confirmed on hvx, and reproduced with Docker (`native/`, a shim that sets the server's wall clock
back), not posted yet:
- After a backward clock step, a restart of the server holds accepted invocations until the wall
  clock reaches their creation time. The vqueue scheduler starts an inbox entry only when its
  `run_at` is not later than the wall clock, and a failover keeps the creation time as `run_at`.
  A one-way send then waits the step's size: `disk=none dseed=d584008bda670834 | run P=svc@28+2
  T=30:-300 W=726d723b,p1,q4,r600 L=restate@30+1`. Without `T=`, or without the restart, it passes.

Durable timers that follow the wall clock are documented. The explorer reads an image's manifest
with its parameter values now, so a `guest:` key can name a parameter.

**Apache Iggy (M314, tentative number).** Iggy server 0.10.0 runs as one server from its image in
`hydo/onboard/examples/iggy` (RESULTS.md), imported with `hydo compose`, with its data on a file
system of its own, on hvx. A client on Iggy's Go SDK v0.10.0 sends messages with unique ids to a
topic of 3 partitions. A consumer group reads them with a partition reader and stores its offsets
by hand. A messages check judges no loss, no duplicate, the log's order, both readers'
deliveries, and no redelivery after an acknowledged offset store. A check of a second topic
judges its message expiry: no message gone before its time, and no expired leading segment kept.
With the durable setting `durability: persisted`, no promise broke in 112 blind schedules and 30
directed runs. The faults were kills, pauses, power cuts at steps, at the client's
acknowledgements and at disk flushes, the server's power loss, damaged files, and clock steps.
No new bug:
- With Iggy's default `replicated`, `hydo explore --configs` lost acknowledged messages after a
  kill alone (`K=iggy@15+1`) and after a power cut alone. It labeled both the documented
  trade-off (Iggy's README; issue #4631).
- A clock effect: after a backward step, the oldest segment's timestamps lie ahead of the clock.
  The cleaner stops there, so a partition keeps all its data for the step's length.
- Detected damage of a segment, WAL, or frontier file fences the partition, as documented.
**The VictoriaMetrics cluster (M312, tentative number).** VictoriaMetrics v1.156.0 runs as two
`vmstorage` nodes, one `vminsert` with `-replicationFactor=2` and one `vmselect` with
`-dedup.minScrapeInterval=1ms`, from the official images in one hvx guest, in
`hydo/onboard/examples/victoriametrics` (RESULTS.md), imported with `hydo compose`. Each node's
data is on a file system of its own. A Go client imports new series and records each
acknowledged sample; a messages check over the export, a query check, and a read during the
faults judge them. Without clock steps, losing one node lost no acknowledged sample, and every
query returned each sample once, with its value. Three new candidates, all clock effects, not
posted:
- A backward clock step deletes the current month's data on every node: since v1.141.0 a
  watcher drops each monthly partition that starts after now plus `-futureRetention` (2 days).
  `T=20:-1728000` deletes every sample acknowledged before the step; v1.140.0 keeps them.
- After a backward clock step, new samples and index entries wait in vmstorage's memory for
  the step's length: their flush deadline is in wall-clock milliseconds. They are not
  searchable, and a crash of both nodes 40 s after a step back of an hour lost 360 acknowledged
  samples instead of the last 60.
- At a backward clock step, vminsert's writes to every node fail with `i/o timeout`: the
  connection compares its deadline with a cached clock that lags up to a second. A batch then
  gets one copy, and a power loss of that one node loses acknowledged samples.
**ClickHouse's replicated inserts (M311, tentative number).** ClickHouse v26.10.6.18 runs as two
replicas of one shard with three ClickHouse Keeper servers in `hydo/onboard/examples/clickhouse`
(RESULTS.md). It runs from the official images, imported with `hydo compose`, on hvx only. A Go
client writes blocks into four `ReplicatedMergeTree` tables. They take synchronous inserts
deduplicated by hash or by `insert_deduplication_token`, async inserts, and quorum inserts with
`insert_quorum = 2`. The quorum inserts run in the parallel mode and in the sequential mode. The
client retries each insert on either replica and sends one acknowledged block of three again. A messages check over both replicas and
two final rules judge them, with a read after each quorum acknowledgement and reads with
`select_sequential_consistency = 1`. No retry stored a block twice. Every acknowledged quorum
insert was on the other replica at once. With the fsync after an insert and a merge on, no
acknowledged insert was lost in a run that reached its checks. `hydo explore --configs` over the build parameter `fsync` showed the
default settings' documented trade-off: a power cut lost a block acknowledged with
`insert_quorum = 2`. One known bug, not
posted:
- With `fsync_after_insert = 1` and the fsync after a merge and a fetch off (the defaults), a
  power cut loses merged parts on both replicas. Each replica queues a fetch that no replica can
  serve, and the part check waits for a merge that never comes. Acknowledged rows stay on one
  replica for good: `disk=cut | run C=40` (ClickHouse #29778, #53143). With every fsync setting
  on it passes.

One new candidate, not posted (M325, tentative number):
- A Keeper without a quorum spins on a session that its client closed. The client sends `Close`
  and shuts its socket down. The handler then stops reading the socket, but its `poll()` asks for
  `POLLIN`, which the unread FIN gives at once (`KeeperTCPHandler.cpp` lines 610 and 192 to 220).
  So the thread runs until the session timeout of 30 s. On hvx two such threads of `keeper2` took
  about a third of the guest's CPU after a ring partition of the Keepers.

Four schedules ended `TIMEOUT workload` because hvx's guest clock nearly stopped under that busy
work (M319, `guest.exact_tick`).

**Temporal with business rules (M231).** Temporal 1.32.2 ran on PostgreSQL 16 on the Mac engine
and on hvx. Its order workflow runs activities and a timer, and takes signals and updates.
Without clock steps every rule held. One new candidate is not reported upstream: the SQL handle
limits its reconnects to one per second by the wall clock. After a backward clock step, one
crash of PostgreSQL leaves the server without its database for the step's length. Every start
then answers `Namespace default is not found.` It reproduces natively in a Colima VM. A
backward step also delays every new timer by its size, which the history service does on
purpose. The example is `hydo/onboard/examples/temporal` (RESULTS.md, `native/`).

**What the Lago onboarding lacked (M225).** Six additions, each a format-1 or results addition
that changes nothing for a manifest without it:

- An `eventually` check with `after: commands` (`# hydo: after=commands`) counts from the end of
  the `eventually` commands, which then run before it.
- `test.first_records: true` keeps the `first` commands' `$HYDO_ACKED` and `$HYDO_OUTCOMES` lines
  in the run.
- A failing check's lines before its last one go into a `HYDO-CHECK-OUT` console note, which no
  fingerprint covers. They reach `result.json` (`findings[].details.output`), `hydo report` and
  `report.md`.
- `hydo explore --shrink-causes N` shrinks several failures of one signature with other fault
  kinds (`Shrink.fault_kinds`).
- `services[].job: true` keeps the explorer's faults off a one-shot job; `hydo compose` writes it.
- `T=<at>:@<point>` sets the clock to a time of day or a calendar boundary (`@:10-2s`,
  `@month+10m`, UTC), and `clock_points:` lets the explorer draw such steps.

The references are four sections of MANIFEST.md (Liveness, Command kinds, One-shot jobs, and
Clock steps to a time) and `hydo guide checks`, `composer` and `faults`.

**What an onboarding of a project's own binaries lacked (M239).** An agent onboarded a user's
own system, built from source, on a Mac with 0.3.1 and named its friction. The fixes, none of which moves a
recorded value:

- `ready: tcp:<port>` (or `tcp:<host>:<port>`), a format-1 addition: a connect check, run as
  `nc -z -w 2 <addr> <port>` with the guest's busybox (`hydo_manifest::ready_command`).
- A failed run's diagnosis replay writes the last 64 KiB of each service's log, without color
  codes, to `<run>/logs/<service>.log`. The `hydo: diagnosis:` line quotes whole last lines and
  names the file.
- `hydo run --memory` prints each service's peak memory at the run's end, read by one more replay.
- `hydo run`'s refusal for memory names the running `hydo inspect` sessions, and `hydo inspect
  start` says that its session holds the guest's memory.
- `hydo build --check` warns about missing `root.files` sources.
- From the Valkey onboarding:
  - `hydo report --details <run dir>` prints what each check said the last time it held;
  - the property table tags a project's check `[the project's own check]`;
  - the lock file names `hydo compose`;
  - the commit advice comes only in a git repository;
  - `hydo setup` says so before it moves `~/.hydo/bin/hydo` to another version.
- The guide has the topic `minimum-path`, the static musl recipe for Rust on a Mac, and the
  messages records' value rules. The skill (2.0.1) finds an installed hydo that is not on `PATH`.

The details are in `docs/HISTORY.md` Milestone 239.

**What a user's first explorations of a message queue taught (M240).** Three of four failed
explorations came from the plan, and the guides now say how to avoid each:

- `hydo guide messages`, "Write the drain": the drain outlasts every lease and the system's
  recovery after a power cut, retries on errors, and fits inside `workload.timeout`, which
  bounds every `final:` read-back. The guide has a shell drain to copy.
- An `eventually_` command runs once and retries by itself; hydo polls an `eventually_` check
  (`hydo guide composer`, `checks`, and the skill's step 3).
- A system that measures leases or timeouts on the wall clock fails under a backward clock step.
  That is a bug of the system: keep the finding and keep `clock` in `faults:` (`hydo guide
  faults` and `messages`). `guest.clock_after_cut` is for systems that record leases too.

The report shows a long clock step also in hours and minutes. The skill is 2.0.2. Nothing
recorded moved. The details are in `docs/HISTORY.md` Milestone 240.

**Upgrades (M238, tentative number).** A service may list the versions it can run
(`services[].version` and `services[].versions`, a format-1 addition). The guest token
`U=<target>@<at>[/<gap>][+<back>]:<version>` switches it during a run. hydo stops the service with
a SIGTERM and a SIGKILL 10 s of guest time later, and starts the new version on the same data. A list or a group selector is a
rolling upgrade, and `+<back>` rolls back. A member that does not start on its new version ends
the run with `VIOLATION [upgrade.started]`, and a power cut carries the versions to the recovery
boot. `result.json` lists the switches as `upgrades` (a results addition), and `hydo explore
--upgrades` draws them (class `upgrade`). The example `hydo/onboard/examples/etcd-upgrade`
reaches etcd issue 19557 on hvx with `U=etcd1@5:3.6.0`, and etcd's documented path through
3.5.26 passes. Design: `docs/design/upgrades.md`; guide: `hydo guide faults`, "Upgrades and
rollbacks"; details: `docs/HISTORY.md` Milestone 238.

**One node's storage faults on NATS (M235).** `hydo/onboard/examples/nats-jetstream/storage/`
puts each node's store on a file system of its own. On hvx it reaches two of Jepsen's NATS 2.12.1
findings in one VM. `P=n1@2+10 L=n2@10+2 K=n3@12+6` loses acknowledged messages (3.5), and one
node's zeroed metadata snapshot deletes the stream (3.3). The section "One node's storage faults"
of its RESULTS.md has the values.

**Several nodes' storage faults and the strict power loss (M245).** `G=` takes a list or a
group's selector (`G=n1,n3@...`, `G=nodes.majority@...`), and `hydo explore --storage-faults
--corrupt-several` draws them. `L=<svc>@<at>+<down>:strict` turns the guest's periodic writeback
off from the run's start and sets the node's bridge port down while it is down
(docs/design/storage-faults.md). On hvx the NATS example's `blocks/` reaches Jepsen's 3.2. Its
stream has three 8 MB blocks per node. Flipped bits in the first block of two nodes leave the
replicas different for good, and with the intact node killed 2,456 acknowledged messages are
lost. The details are in the section "Several nodes' corrupted blocks" of its RESULTS.md.

**Function contracts (M241 and M255, a prototype on the Mac and KVM engines).** `// hydo:
requires` and `ensures` comments next to a function, with `old()` and `result`, are checked at
every call. The manifest's `contracts:` names the program and its sources. `hydo build` compiles
the plan `<image>.contracts.json` from the program's DWARF (crate `hydo-contracts`). Since M255
an expression reads Rust `String`, `&str`, `Vec`, slices, `Option` and `Result`, Go strings,
slices, results after the first and `error` (`len()`, `v[i]`, `s == "text"`, `some(o)`,
`result.1`). The Mac engine and, since M255, the KVM engine (three debug registers with
`KVM_SET_GUEST_DEBUG`) stop at hardware breakpoints and only read. A run with the plan has the
fingerprint of the run without it, and a broken clause is a `contract` finding and a
`VIOLATION`. A run judges its first 100,000 calls and reports the rest. On hvx, uprobes changed
the fingerprint, and engine breakpoints are designed only. The example is
`hydo/onboard/examples/contracts-ledger`; the design and the measurements are
`docs/design/contracts.md` (open item 113). On Blnk (M260, the Mac engine, 24 blind schedules)
contracts on three functions found nothing that the business rules did not. The overdraft
contract broke in the same runs as the rule and named the call and its values. It could not
state the cause, a `float64` limit lost in a map, and no contract over memory states the atomic
bulk (the Blnk example's RESULTS.md, "Contracts against rules").

**Function fuzzing from contracts (M275, a prototype for Rust, natively).** `hydo fuzz
[CRATE_DIR]` tests each contracted Rust function on its own. It generates inputs from a seed
that keep `requires` and the `input` lines (now also field paths, `input l.balance in ...`), with
edges from the clauses' constants and related paths. It calls the function in a copy of the
crate through a one-line test module written after the function, and judges each call by
`ensures`. A broken clause or a panic shrinks to a small input, and `--input` replays it. On the
contracts-ledger example it found the planted bug from the contract alone, and two gaps in the
contract. On 12 functions of Qdrant's `common` crate nine held. `decompress_sequence` panics in
the test profile on a header that `compress_sequence` never writes, on the path that decodes
untrusted sparse vectors. Design: `docs/design/function-fuzzing.md` (open item 139). An
evaluation on six popular crates (M280, `docs/design/function-fuzzing-evaluation.md`) found that
a new user would not keep using it yet. M282 fixed what it found, and the six crates ran again:
- The harness builds on `#![no_std]` crates and reads results with private fields of other
  modules. A function whose harness does not build is reported, and the others run.
- Each function ends with `held`, `broken`, `vacuous` or `too few inputs`; the last two exit 3
  with a `to fix:` line (`--min-inputs`, `--min-reach`).
- `input` lines take `len`, `chars`, `like` examples and list elements (`v[]`). Lists grow to
  256, with edge sizes to 4,096, and sizes stay within 2^20 without an `input` line.
- `with T = i64` fuzzes a generic function. `hydo guide fuzz` is the user's reference.

Every harness of the six crates built. memchr's own contract found the planted bug, and `gcd`'s
found num-integer#55. The verdict: kept for functions over integers, strings and lists, not yet
for APIs of traits or of types that hold pointers.

A second evaluation by a fresh user (M288, `docs/design/function-fuzzing-evaluation-2.md`)
found that a clause on an `f64` result counted as `held` on 0 decided calls. It also found that
the harness failed on `#![forbid(unsafe_code)]` and lost the `[lints]` table. M289 fixed them:
- `f32` and `f64` values are read, compared exactly with integers, and generated. A clause
  decided on fewer than `--min-reach` calls is `not judged` (exit 3), never `held`.
- When only panicking calls could reach a clause's last part, the clause is `not reached` and
  the verdict is the panic, with no advice to change the inputs.
- The test module allows no lint that the crate forbids, and the copy keeps the `[lints]`
  tables. Each contracted function that the run does not build a harness for stays in use.
- When the copy fails but the crate builds with its own `Cargo.toml`, the message names hydo's
  copy as the cause.
- A contracted function that is not fuzzed makes the exit status 3 (`--allow-skipped`).
- `like b"..."` gives examples for `&[u8]` and `Vec<u8>`, which reports show as byte strings,
  and `--show-inputs N` prints and keeps some of the inputs.

strsim's `jaro` is judged on all 1,000 calls, and strsim and httparse build unchanged.

Since M307 `hydo fuzz` covers the parts of a Rust API that both evaluations found refused:
- Enumerations of the crate and `Ordering` are generated and read. `matches(e, <pattern>)`
  names a variant (`matches(result, Ok(Complete(_)))`), and `ok(result).Complete.0` reads its
  field.
- `with Self = <type>` fuzzes a trait's method on that type, once per line, required methods
  and default ones. `with` lines also name a generic `impl` block's parameters.
- `build <Type> from <constructor>(<name>: <type>, ...)` and `read <Type> as <type> by
  <method>()` make and read a type with pointers or private fields, such as bytes' `Bytes`.
- `requires sorted(v)` makes the generator sort the list.

On fresh copies of the second evaluation's six crates and byteorder, 59 of 66 contracted
functions were refused before and 2 after (`docs/design/function-fuzzing-evaluation-2.md`,
"After Milestone 307"). The broken contracts were earlier findings, now stated on the API itself.

A third evaluation by a fresh user on six other crates (M308,
`docs/design/function-fuzzing-evaluation-3.md`) found the tool worth keeping for property
functions and simple contracts. hydo fuzzed 57 of 73 contracted library functions and reproduced
9 bugs already reported upstream, none new. One verdict was hydo's own false `broken`.

Since M318 (tentative number) the first six fixes of that evaluation are in, each its own
commit:
- An element or a field that a `build` line builds is compared as its `read` line reads it, and
  a comparison hydo cannot make is `not judged`, never `broken`.
- `Self::Owned` and other associated result types are read, and a clause whose last part reads
  a value of a type hydo does not read is `not judged` with that type named.
- A name of two modules resolves by the function's module, its file's `use` lines, or a path.
- Const generic parameters take `with N = 8`, `&'static` parameters get a leaked value, and a
  parameter bound by `Fn`, `FnMut` or `FnOnce` takes a generated closure (`|x| x > 0`).
- A third of the random strings hold characters whose case mapping changes them (`ß`, `İ`,
  `ﬄ`).
- `read` lines take `to_string()` in a `#![no_std]` crate, `collect()`, and turbofishes, and
  `build` lines take generic types (`ArrayVec<u8, 8>`).

On fresh copies of the six crates with 71 contracts, 21 functions were refused and 9 verdicts
were wrong before, and none after. All nine known bugs showed, against seven before (the
evaluation page, "After Milestone 318").

Since M326 (tentative number) the rest of that evaluation's list is in, each its own commit:
- A test module that does not build names the error's cause in the user's files (a `build`,
  `read` or `with` line, a parameter, the call), with the compiler's `help:` lines. The whole
  output is in `<out>/harness/<function>.log`, which `--show-harness` prints.
- A private field's refusal names the `build` line with the crate's public constructor and its
  parameters, and a `read` line.
- Contracts take the bit operators, `requires len(a) == len(b)` generates two lists of one
  length, and `matches()` takes `Name { field: <pattern>, .. }`.
- `--fn codec::` selects a module, `&'static mut` parameters are fuzzed, names resolve through
  glob imports and re-exports, and a workspace member's own error names the member.
- Closures take two arguments (`|x, y| x + y`), a sort's comparison (`|a, b| a.cmp(b)`), and an
  `Option` result.

On the six crates every verdict and every smallest input stayed, and all nine known bugs showed
(the evaluation page, "After Milestone 326"). Open item 163 has what is left.

A fourth evaluation by a fresh user (M327, tentative number,
`docs/design/function-fuzzing-evaluation-4.md`) used semver, base64, smallvec, time, textwrap
and ordered-float. It found the tool worth keeping for property functions and for contracts over
numbers, strings, and enumerations. hydo fuzzed 71 of 88 contracted library functions and all 60
property functions, and reproduced 3 issues already reported upstream, none new. Generic
containers (`A::Item`), parsers whose error has private fields, and types that only a fallible
constructor builds still stop contracts on the API. One verdict was hydo's own false `broken`.

Since M333 (tentative number) the fourth evaluation's fixes are in, each its own commit:
- A comparison of values that hydo does not read is `not judged`, with the type named.
- A type parameter's associated type (`A::Item`) resolves through its `with` line's type and
  the crate's `impl` of the trait, in parameters, results, and closures.
- An error with a field of a private module builds: the test module writes such a type by its
  path from the crate's root (`crate::error::ErrorKind`).
- A `build` line takes no arguments (`X::default()`), and a constructor that returns a `Result`
  or an `Option`, whose `Err` or `None` throws the input away. The refusals propose only lines
  that build, also for types of other modules and of generic `impl` blocks.
- A generic structure whose fields are `pub` is generated with the `with` types, and `with T =
  &[u8]` passes a slice. Structures with lifetimes, `Cow<str>`, and a built type's public fields
  are read; two values of one enumeration compare with `==`.
- The smaller ones: `--features`, `--fn <Trait>::`, the crate's library name in reports, two
  NaNs together, an index inside its list, and a refused `with` line of no type parameter.

On the evaluator's first contracts, without helpers, hydo fuzzed 76 of the 88 contracts against
51 before, and with the lines that the refusals name, all 88. No verdict was hydo's own error,
and the three known issues showed again (the evaluation page, "After Milestone 333").

**Function fuzzing inside the VM (M292 and M296, tentative numbers; a prototype on hvx and on
the Mac engine).** `hydo fuzz --vm [CRATE_DIR]` makes the calls inside hydo's VM under faults: power
cuts (`C=`, `cf=` on hvx), disk errors, clock steps, pauses, and since M296 kills. It builds the harness as a static musl program and writes a
project. The project's workload makes the calls, one step per batch, each call in an empty
folder of its own on the data disk. hydo builds its image and runs it under drawn schedules.
`// hydo: durable <check>` names a property function that runs after the run, in the recovery
boot after a power cut. It runs once for each call that started, with the call's input and its
result as an `Option` (`None`: the call did not return). A failure shrinks alone in a run of one
step under the same faults, and `hydo run <image> '<schedule>'` replays it. The example
`hydo/onboard/examples/fuzz-vm` has three functions:
- redb 4.6.0's commits held under every power cut, also in the middle of a commit;
- a save by rename with tempfile 3.27.0 and no sync lost its text at every cut after the call,
  shrunk to `text = ""`;
- the same save with two syncs held.

The verdict of M292: keep it for functions that write files and promise durability or atomicity.

Since M296 most drawn schedules put the fault inside one call. `E=<step>.<call>` names the call,
which reports its start as the event `fuzz.call` and names its thread `hydo-call`. The reaction
`A=fuzz.call+<ms>:<action>` then cuts the power, steps the clock, stops the call's process or
its first thread, or kills it. The reactions gained the actions `clock+<s>`, `stop-workload-<ms>`,
`stop-workload/<thread>-<ms>` and `kill-workload/<name>`. On hvx and on the Mac engine:
- a wait whose deadline reads `SystemTime` broke under a forward clock step inside the call, and
  its twin on `Instant` held;
- a count that waits for its worker with a timeout broke under a pause inside the call, and the
  twin that joins held;
- rs-snowflake 0.6.0's `real_time_generate` gave a smaller id after a backward step of 30 s, a
  candidate not posted; crossbeam-channel 0.5.17's `recv_timeout` held.

The Mac engine repeated M292's results: redb held, the unsynced save lost its text, and the
synced save held. Each shrunk finding replayed with its fingerprint. The design and the
evaluation are the section "Inside the virtual machine" of `docs/design/function-fuzzing.md`.

**Value history (M285, tentative number; a first version on the Mac engine).** `// hydo: key
<expr>` and `// hydo: record <expr> [as <name>] [at entry|at return]` next to a function record
values at every call, through the contracts' plan and breakpoints, so a run with the plan has
the fingerprint of the run without it. `hydo inspect values <run dir> [--key K] [--changes]
--text` shows each key's values in the run's order with each change marked. A key that extends
another after `_` (`d17_q`) is in its history. `hydo inspect events` lists them as `value`
events, and `hydo report --html` has a section on how the recorded values changed. The new
readers are these:

- `float64` and `float32`, which clauses now compare too;
- strings;
- a Go interface's dynamic value, by its runtime type descriptor;
- a Go map's value by a constant key, in a map of at most 8 entries.

Each record also names its caller from the return address. On Blnk's overdraft regression the
history showed the limit at 50.0 in the API and absent from the saved metadata. It was 0.0 in
the worker's coalesced copy of the two draws the rule found. The fixed build kept 50.0
throughout. The plan left the fingerprint as it was, and its 142 breakpoint stops cost less
than the runs' spread. The design is `docs/design/value-history.md`, the guide `hydo guide
values` (open item 146).

**Value properties (M291, tentative number).** `// hydo: keep <name> [once set] [from <function>]
[to <function>] [across boots]` next to a recording function says what must hold for a recorded
value along each key's history. Without `once set`, a judged stop has the value of its reference
stop: the last stop of `from`, or the key's first. With `once set`, a value once present is
never absent again. The host judges the lines from the run's records, so the fingerprint stays.
A broken line is a finding of the kind `values`, and it turns an `OK` verdict into `VIOLATION
[values <the line>] the value changed`. The verdict names no key, so a shrink keeps one
signature. The finding's output is the first broken key's history up to the break, with the
first changing call marked. `hydo report`, `hydo report --html`, `hydo inspect values --broken`
and the advice show it. On Blnk, `keep overdraft_limit from blnk.setTransactionMetadata to
model.canProcessTransaction` fails the run `disk=none | run`, where every business rule held: 3
of 66 keys reached the decision with the limit 0.0. The fixed build holds. Each schedule gave one
fingerprint with and without the plan. In 30 blind schedules the line broke in 9, first at seed
4, and the overdraft rule in 1, at seed 21. Seed 4 shrank to a schedule without a fault. The
design is the section "Value properties" of `docs/design/value-history.md`, and the how-to is
section 4 of `hydo guide values` (open item 146).

**Error paths (M306, tentative number; a first version on the Mac engine).** `error_paths: true`
in a `contracts:` entry (a format-1 addition) makes `hydo build` list the program's
error-handling places. They are Go's `if err != nil` bodies, Rust's `Err` returns and arms, `?`,
`unwrap()` and panics, and functions named for a retry, a recovery, or a rollback. `// hydo: error
path` adds a line, and `// hydo: no error path` removes a line, a block, or a function. The Mac
engine writes a one-shot `BRK` over each place's first instruction in guest RAM while the guest
runs. It puts the instruction back at the first hit and counts no exit, so the fingerprint stays.
`result.json` and `exploration.json` get `paths` (results additions). `hydo report --paths [DIR]
[--against DIR]` lists what only faults reached and what never ran, by function. `hydo report
--html` has the section on which error paths ran. `hydo explore --no-faults` runs each seed's
workload without a fault, the baseline. On Blnk (1,326 places), 73 ran in 12 schedules with
faults and 27 without. The 46 that only faults reached were database errors under kills of
PostgreSQL, queue errors, and the recovery of queued transactions. Every schedule gave one
fingerprint with the plan and without it, and a run's search of guest RAM took 10 ms. The design is
`docs/design/error-paths.md`, the how-to `hydo guide error-paths` (open item 155).

**Faults of the disk's blocks (M313, tentative number; the Mac and KVM engines).** Four host
tokens make faults that a real disk makes, on the data disk, decided from `dseed=` and from the
requests counted from the workload's start:
- `torn=<p>[:any]` (with `disk=cut`) keeps a prefix or a subset of the sectors of unflushed
  writes at a power cut;
- `mis=<k>[:<sectors>]` sends the k-th write to another place;
- `lse=<sector>+<count>[:rot]` makes reads of sectors fail or return damaged bytes;
- `hang=<k>[+<ms>]` leaves the k-th request without an answer, or fails it after `<ms>`.

`G=<svc>@<at>:bad|rot` puts bad sectors under a range of a file of a service's storage. The
controller maps the file's ext4 extents through the agent's `read` and sends the engine's new
command `DISK lse=...`; no agent or image changed. A workload that times out in a run with
`hang=<k>` ends it with `VIOLATION [disk.hang]`, and a guest that waits 120 s on such a request
ends it with an ERROR. The faults' state joins a fingerprint only when a token armed it, so the
release's Redis example gave its 7 recorded values. `hydo explore --block-faults` draws them
(class `damaged-disk`); hvx refuses them before any VM. The example
`hydo/onboard/examples/disk-faults` (RESULTS.md) shows each fault breaking a careless record
store and a careful one passing. The design is `docs/design/block-faults.md`, the how-to `hydo
guide faults`, "Faults of the disk's blocks" (open item 160).

**Fakes from OpenAPI descriptions (M249).** `hydo fake from-openapi <spec>` writes a `fakes:`
entry from an OpenAPI 3.0 or 3.1 description (the module `openapi` of `hydo-compose`). It writes
one route per operation, with the spec's example or a body made from the schema, and no random
value. Templates (`{seq}`, the new `{uuid}`, `{path:x}`, `{json:x}`, `{form:x}`) stand where the
schema allows. `--stateful` adds `save:`/`load:` to create-then-read pairs. The idempotency header
gives `idempotent: true`, and the spec's error answers fill the route's `errors:`, a format-1
addition that an `H=` rule `status=<code>` answers. The command prints, writes (`-o`) or adds
(`--add-to`) the entry, and refuses unmappable operations with one line each. Goldens: the OAI
Petstore and Adyen's Transfers API. On the Mac engine the example
`hydo/onboard/examples/openapi-payouts` showed its planted retry bug, and every replay gave its
fingerprint. The design is `docs/design/openapi-fakes.md`; the details are in `docs/HISTORY.md`
Milestone 249.

**From Kubernetes manifests (M243).** `hydo k8s <files | folder | chart | kustomize folder>`
writes a starting `hydo.yaml` from Kubernetes manifests, a Helm chart (`helm template`) or a
Kustomize folder (`kubectl kustomize`); `hydo onboard` runs it when there is no Compose file. It
converts the objects into a Compose-shaped document with `x-hydo-*` keys and hands it to the
Compose importer (the module `k8s` of `hydo-compose`, so no new crate). A StatefulSet gives one
service per replica and a group; init containers, Jobs and Helm hooks become one-shot jobs;
Services become names in `/etc/hosts`; probes become `ready:`. Secret values go to a git-ignored
`k8s-import/secrets/` and reach the image through the format-1 addition `services[].env_file`.
The mapping and the refusals are in `docs/design/k8s-import.md`; the proof is in
`docs/HISTORY.md` Milestone 243.

**Kubernetes in the guest (M246, an experiment on hvx).** k3s v1.37.2+k3s1 runs as one hydo
service on hvx, with no change to hydo, the module, or the guest kernel. The image carries the 57
kernel modules that k3s needs, from the same nixpkgs build as hvx's `bzImage`. The node is Ready
about 8 s of guest time after k3s starts, and a StatefulSet of 3 pods is ready about 22 s later.
Two fresh boots of a schedule give one fingerprint. In `hydo/onboard/examples/k3s-lease` the
pods elect a leader on a Lease. A forward clock step gives two leaders at once, and a leader that
crashes in a loop hands over. The explorer found that a step back of more than an hour leaves no
leader, because the clock goes before the cluster's certificates.

**The guest kernel for containers (M256).** `guest.kernel: containers` (a format-1 addition)
gives an image a guest kernel with what containers and Kubernetes need, all built in: the Mac
engine's `Image-explore-containers` and the KVM engine's `bzImage-kvm-containers` and
`bzImage-kvm-smp-containers`. The image's record names the variant (`kernel_variant`), and an
image without the key keeps its bytes, so nothing recorded moves and the epoch stays 3. On hvx
the key changes nothing: its kernel has the options, the network ones as modules. On the Mac
and KVM engines runc runs a container with memory and pids limits, and NAT, conntrack, and ipsets
work. kine loops without VM exits, which an exit clock cannot interrupt, so k3s does not start
on the Mac. On KVM with `guest.branch_tick` and 2 vCPUs it does since M274: the node is Ready, the
pods elect a leader, and a schedule repeats its fingerprint (open items 13 and 119). The example is
`hydo/onboard/examples/k3s-lease/containers`; `docs/design/k3s-in-guest.md` (open item 119).

**Ready-made emulators of outside services (M242).** One `emulators:` line gives a project Stripe
(stripe-mock), SMTP (Mailpit) or S3 (`rclone serve s3`). The validator expands each entry into a
service `<name>_emulator` from the emulator's image and a fault proxy `<name>` in front of it, so
`H=`, the record and `hydo-world check` work as for a fake. `hydo build` fetches the images by
pinned digests into `emulators/` beside the manifest; hydo ships none of them. The explorer
draws no fault on an emulator's service. An `http` proxy route takes `idempotent: true`: a
repeated key gets the first answer, recorded as a replay. On hvx the example
`hydo/onboard/examples/checkout` found its planted double charge blind (5 of 200 schedules), and
every replay gave its fingerprint. The design is `docs/design/emulators.md`; the details are in
`docs/HISTORY.md` Milestone 242.

**The clock audit (M248).** `hydo rules scan --kind clock` lists the code that measures a
duration with the wall clock: a lease, a timeout, a throttle, a lag, or a delay. The audit reads
eight languages and SQL as text. A reading is followed through functions, variables, fields,
and parameters. Each place gets three clock-step schedules. The report ranks one place per function
and shows the 5 most likely; `--all` and `--json` show every place. The audit found the known lines of
Valkey, Temporal, and River at their tagged versions. On the Mac engine its schedule for Valkey
gave the fingerprint of the example's regression for valkey-io/valkey#5406. The skill (2.3.0)
runs the audit during onboarding and offers the first three places. Since M324 it leaves out
test code inside source files and ranks first the code that the services of `hydo.yaml` run.
The guide is the section "Find the durations measured on the
wall clock" of `hydo guide faults`. The design and the precision numbers are in the section
"The clock audit" of `docs/design/rule-suggestion.md`.

**Web apps through their Playwright tests (M253).** `browser:` (a format-1 addition) runs a team's
Playwright tests unchanged as the workload, in Microsoft's Playwright image 1.65.1, which `hydo
build` fetches by digest into `browser/`. Each workload step runs one shard. A watcher loaded with
`NODE_OPTIONS=--require` watches every page the tests open. Five invariants judge what it saw: a
failing test, an uncaught error or `console.error`, a spinner that stays, a failed request the page
never shows, and texts such as `NaN`. The API can be the real backend or a fake with `H=` faults.
The example `hydo/onboard/examples/web-shop` (React, a fake API) ran on hvx with one fingerprint per
schedule. Its flaky test fails on every run of `H=api.add_item@1-2:delay=1000` and passes on every
run without it. `hydo explore --protocol-faults` found its stuck spinner and unhandled rejection
blind (seed 7 of 40). It needs `network: bridge`, and since M284 it runs on every engine. hydo-build now leaves out
layer entries whose names have a backslash or a quote instead of refusing the image. The design is
`docs/design/web-apps.md`, the how-to `hydo guide web-apps`, and the details are in
`docs/HISTORY.md` Milestone 253. Since M254 a passing test counts as acknowledged work, and the
pages' screenshots go to the run directory's `screens/` after the verdict, where `hydo report
--html` shows them under the finding that names them. `browser.screens: compare` compares each
page's screenshot with a recorded reference, pixel for pixel (`<name>.screens_match`). The
example ran on KVM in M254, with hvx's verdicts and the same screenshots on Intel and AMD hosts. Since M284
the Mac engine runs `browser:` and the walk: the image's root is a read-only ext4 file system on
its disk (`guest.root: disk`), which a small initramfs mounts. The guest kernel did not change,
and no run ended in `SPIN`. On the Mac the example web-shop's flaky test, both planted bugs of
its walk (found blind in seeds 1-20), and Memos's walk with its recording gave hvx's verdicts. Each
schedule gave one fingerprint (`docs/design/web-apps.md`, section "The Mac engine").

**hydo walks through a web app's pages itself (M258).** `browser.explore` (a format-1 addition)
replaces `browser.tests`: no Playwright test is needed. hydo's walker (`walk.mjs`) runs in the
Playwright image with playwright-core 1.65.1, which `hydo build` fetches by its sha256. The same
five invariants watch it. Each workload step is a fresh browser that keeps the session. Each
action reads the page's accessibility tree and does one thing, such as a click, an edge value in
a field, a save, or a double click. The schedule token
`I=<seed>[,n<k>][,x<step>.<k>][,rec]` names the walk, and every action draws from its own stream.
`hydo explore` draws a walk for every schedule, and the shrinker lowers the actions per step and
leaves actions out. A walk writes the server's acknowledged writes to `$HYDO_ACKED` for a
project's own rules. Every run leaves the walk as a Playwright test, `screens/repro.spec.ts`, with
`page.route` for `H=` faults. With `,rec` a run also records a video and a trace of the last
step; `hydo explore` makes one such replay of each minimal walk. On hvx the web-shop example
without its tests (`hydo-walk.yaml`) had both planted bugs found blind in 20 schedules. The
minimal walk's test fails natively on the buggy build and passes on the fixed one. Memos 26.11.0
(`hydo/onboard/examples/memos`) is onboarded with it. In 40 focused walks with faults, 19 of them
power cuts, every one of 85 saved memos survived. There are two new candidates, not posted:

- over plain HTTP from another host, a share link's copy button throws (`navigator.clipboard` is
  undefined);
- a reaction the server refuses disappears without a message.

The design is `docs/design/ui-exploration.md`, the how-to `hydo guide web-apps` ("Let hydo walk
through the pages"), and the details are in `docs/HISTORY.md` Milestone 258.

Since M264 a walk shrinks by whole steps, halves of a step, and single actions
(`I=<seed>,x<step>.<first>-<last>`), with a budget of runs. A page invariant's signature has
`<id>` for a generated id. Memos's share-link walk shrank from 72 actions to 31. The walk
chooses an action's kind by weighted rendezvous hashing. It reads a time in a control's name as
one word. A clock step that a shrink drops now moves 9 of the 72 actions of Memos's seed 46; it
moved 24. A `watch.failures_of` that matches no request warns at the run's end. The hvx engine
keeps 16 MiB of the agent's lines per boot, and a run copies at most 1 MiB of files. So `,rec`
no longer ends in `TIMEOUT check`, and it records the last step that takes an action. On KVM
both examples gave one fingerprint per schedule on four hosts. The details are in
`docs/HISTORY.md` Milestone 264.

Since M267, when a menu or a dialog is open, an action most often acts in it. A fill takes only
an editable field, and a click only a control that takes Playwright's trial click. The walk
counts the families it used per step. A dropped clock step moves 7.4 of a walk's 72 actions on
average over five Memos walks, against 19.8 for M264's walker. The blind exploration of Memos still reached neither finding (open item 131). A
finding's recording replay records the step that the finding names (`I=<seed>,rec<step>`). The
details are in `docs/HISTORY.md` Milestone 267.

Since M270 a walk with `rec` repeats the actions that the same schedule without `rec` took,
instead of choosing them again, so a recording shows the failing run. Every run prints its
actions as `hydo-web: walk-act` lines. `hydo explore` takes them from the minimal schedule's
confirmation, and `hydo run` runs the schedule without `rec` first, into `walked/`. Each action
finds its control by the same role and name, and a control that is not on the page is left out
with a line that says so. The details are in `docs/HISTORY.md` Milestone 270.

**Vikunja, the second web app of the walk (M273, tentative number).** Vikunja 2.8.0, a
self-hosted task manager (one Go server with its Vue frontend, SQLite), runs in
`hydo/onboard/examples/vikunja` (RESULTS.md), imported with `hydo compose`. The walk writes
`<METHOD>:<path>=<id>` to `$HYDO_ACKED` for each id that a write's JSON answer returns.
`watch.failures_of` also matches `<METHOD> <path>`, so it can name the writes of an API whose
reads use the same paths. Two rules read the words and the database. The first says that every
task, project, comment, label, saved filter and team that Vikunja answered as saved is in it.
The second says that a Kanban board's done bucket holds exactly the done tasks. The blind
exploration on hvx found two new candidates, not posted:

- A power cut loses the tasks that Vikunja acknowledged in the last seconds. Since v2.2.0 its
  SQLite runs in WAL mode with the driver's `synchronous=NORMAL`. The shrunk walk types one
  task, and `C=6` loses it.
- A task of the quick actions dialog that the server refuses is lost with its typed title, and
  the page leaves the error unhandled.

In 40 walks with every fault kind but the lying disk, 5 of 19 power cuts lost saved objects,
and the other kinds (kills, pauses, network faults, clock steps) lost none. The details are in
`docs/HISTORY.md` Milestone 273; open item 137 has what the walk lacked.

**The failing run's own frames, and the pages read back (M334, tentative number).** Two parts,
each its own commit:
- Every run of a walk keeps a filmstrip. After the page opens and after every action, the walker
  saves a 480 by 270 JPEG of the page and one line in `screens/film.jsonl`. The line names the
  action, the path, and the errors and failed requests since the action before. A failing run
  also keeps the Playwright trace of every step.
- A power cut does not lose them: the controller copies the filmstrip and the last step's trace
  before the cut. `hydo report --html` shows the filmstrip as the run's timeline, step by step.
  The page of an exploration shows each failure's filmstrip in its details.
- An image whose walker has them (the file `/etc/hydo/walk-film`) copies up to 5 MiB per boot,
  each file up to 3 MiB, in windows of 16 reads. An image built before copies as before, at
  most 1 MiB. The Mac and KVM engines keep 32 MiB of console per boot (4 MiB before).
- `browser.read_back` (a format-1 addition) adds the check `<browser>.acked_shown`. It runs at the
  end of the run and in the recovery boot after a power cut. It opens the pages and looks for
  every object whose id the server returned in an acknowledged write.
- On hvx Vikunja's `disk=none | run C=5 I=1` failed it from the pages alone. The home page lacked
  the tasks 3, 4, and 5 that the database rule found lost. The run directory kept the first
  boot's 66 frames and its trace.

Images built from `browser.explore` get new bytes (the walker and `/etc/hydo/walk-film`); none is
in `hydo/onboard/recorded.json`, and nothing recorded moved. The reference is `hydo guide
web-apps`; the design is the last section of `docs/design/ui-exploration.md` (open item 168).

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

**Pauses of threads (M226).** `P=workload/<thread>[#<k>]@<at>~<off>:<dur>` stops only the
workload's threads of that name (`/proc/<pid>/task/<tid>/comm`), or the k-th of them, and the
other threads run on. The guest kernel's cgroup v2 freezer stops them; the agent and the images
are unchanged. Since M234 it runs on every engine for an image built at the fingerprint epoch 3
or later. On the Mac and KVM engines an older image boots kernels without cgroups, and the pause
is refused before any VM with a message to rebuild the image. `hydo explore --pause-thread
<name>` draws it, with `--pause-min` and `--pause-lengths`. On hvx `P=workload/tests::wal_reco@0~4433:500` fails Qdrant's flaky test on
every run. The guide's section is "Pause threads of the workload" in `hydo guide faults`, and
the details are in `docs/HISTORY.md` Milestone 226.

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

**Shared beginnings of runs (M337, research, tentative number).** A tree of snapshots over an
exploration's first runs is a no-go. On 13 hvx explorations of databases and queues, 0.07 % of
12,590 s of first runs repeated an earlier run's beginning. Each schedule draws its own `dseed=`
(which the fingerprint covers), its own `W=` or `I=`, and its first fault early, which splits the
workload's first range. Drawing those once per exploration would save 3.3 %, or 10.9 % with one
workload program per step. It costs search: 69 % of the failing first runs had their first fault
in the first quarter of the steps. Shrinks share more: 10 % to 43 % of their clock, and 10 % to
21 % of their seconds were exact repeats of an earlier candidate. The recommended first version
takes the earlier result for a repeated schedule; the second continues shrink candidates from
their own snapshots on the Mac engine and KVM. The note is `hydo/explore/RESEARCH-shared-prefix.md`
(open item 167).

**Repeated schedules and error shrinks (M339, tentative number).** Three changes for 0.5.0:
- A first run or a shrink candidate whose schedule the exploration already ran takes the earlier
  result and consoles without a VM (`hydo_explore::repeats`). The re-runs and the confirmations
  keep their VMs, and `--rerun-repeats` runs every one. The explorer writes `dseed=0` where the
  disk's seed decides nothing (`disk=none`, and `disk=cut` without a block fault), so drawn
  schedules have new texts. No recorded value or saved regression moved. `exploration.json`
  counts `repeats` and `repeats_saved_s`. On the Mac the Redis quickstart's random seeds 1-40
  answered 4 first runs and 6 shrink candidates, with the same records as with VMs.
- The explorer shrinks a service that does not start again (`ERROR <svc> restart failed`, `ERROR
  restart after the faults`) and a workload or check that does not end (`TIMEOUT ...`, the Mac's
  `exit budget exhausted`) by its signature, as a violation. The system's detection of hydo's
  damage (classes `corrupted-files` and `damaged-disk`) and its error kinds are shrunk only with
  `--shrink-detected-damage` (`report::shrinks`).
- `hydo run` refuses a guest that does not fit into memory with status 5, and with `--json` the
  explorer's JSON line.

The reference is `hydo guide workflow`, sections "A schedule that ran once runs once" and "Which
failures are shrunk"; the details are in `docs/HISTORY.md` Milestone 339.

**A failure as one web page (M220).** `hydo report --html [DIR] [-o FILE]` writes one HTML file
for a run directory, an exploration directory or a `hydo ci run` output directory. It holds its
style sheet and one small script (the copy buttons) and loads nothing. The page shows the
summary, each failure with its minimal schedule and replay command, the failing run's timeline
(the events of `hydo inspect events`), and the first difference from a passing run. That run is
`--against RUN_DIR`, `--diff` (a `hydo diff` or `hydo explain` directory), or for an
exploration the minimal schedule's passing shrink candidates. It also shows the property table,
and for a CI run each finding's state against the base. The code is
`hydo/crates/hydo-cli/src/htmlreport.rs`, and `hydo guide inspect` describes the page in its
last section. Since M228 the page is in plain words for a developer who does not know hydo. It
says what broke and when it happens, as the schedule in sentences (`hydo_schedule::describe`).
It says whether it is a bug, with a label from the failure's class (`hydo_explore::advice`), and
it shows what hydo saw and the command to run in the project's folder. An exploration's failures are problems ranked by attention. Everything for
specialists is in one folded section at the bottom. Since M230 `hydo report` prints an
exploration's problems in the same words (`htmlreport/text.rs`), and `hydo report --details`
prints the technical report. `hydo explore` in a terminal ends with the problems counted. Outside
a terminal, or with `--json`, it ends with the JSON summary line that scripts read.

**Which versions have a finding (M221).** `hydo versions <run dir | explore dir | "<schedule>">`
replays a finding's minimal schedule on versions of the system, oldest first. The versions are a
manifest parameter's values (`--param KEY=V1,V2`), git revisions (`--git A..B`, a checkout and a
`--build` command each), or images (`--images`). Each version is `reproduced` (the same finding
key), `passes`, `fails-otherwise` or `cannot-run`. The search replays the newest and the oldest
version, then bisects, assuming one change point, and names a contradiction. Images come from a
cache by input key. The answer is `versions.json` (the results addition `VersionsDoc`), and
`hydo report` prints it. Reference: `hydo guide workflow`, "Which versions have a finding";
proof: `docs/HISTORY.md` Milestone 221.

**Two versions on the same schedules (M298, tentative number).** `hydo compare-versions` runs
each schedule on version A and on version B and says where the two runs differ. The versions come
as for `hydo versions` (`--param`, `--git`, `--images`). The schedules are given, the saved
regressions (`--regressions`), or drawn from seeds as `hydo explore` draws them (`--seeds`), and
a drawn difference is shrunk with the explorer's candidates. It compares three levels:

- the verdicts and findings;
- the client's observations: `$HYDO_OUTCOMES`, the acknowledged and failed lists, and the
  `HYDO-HIST` records without their guest times;
- the stored state: each service's `compare:` dump (a format-1 addition, `HYDO-STATE` notes after
  the verdict) and what each check printed.

`--ignore`, `--ignore-json` and `hydo/compare.yaml` blank what may differ, and `--again DIR`
compares earlier runs again without a VM. The answer is `compare.json` (a results addition), and
`hydo report` and `hydo report --html` show it. On the Mac engine Redis `appendfsync always`
against `everysec` differs on 4 of 10 drawn schedules. One shrank to `disk=ioerr:1 | run`: with
`always` one failed write stops Redis. Two builds of one source and `hz 1` against `hz 10` agree
on every given schedule. Formance Ledger v2.4.0 logs the account race of its `UpsertAccounts`
rewrite in a run without faults, and v2.3.24 does not; every check holds on both. Design:
`docs/design/differential.md`; details: `docs/HISTORY.md` Milestone 298.

**Which settings break a promise (M299, tentative number).** `configs:` (a format-1 addition)
names build parameters of `params:` and the values to explore, the default first, with
`service:`, `doc:` and per value a `tradeoff:` note of what the documentation says it weakens.
`hydo explore --configs [MANIFEST]` builds one image per combination (`all` up to 12, else a
pairwise set; `one` changes one setting at a time) through `hydo versions`' image cache. It
explores each with the same seeds and prints a table of the classes of failure per combination.
Each class has its minimal schedule and replay command. Per failure it names the values it needs. A
failure found only with values that have a note, and not with a combination without one, is
labeled the setting's documented trade-off. The answer is `configs.json` (results type
`configs`); `hydo report` and `--html` show it. On the Mac engine, Redis's `appendfsync`
(`examples-mac/redis/hydo-configs.yaml`, seeds 1-20) with `always` passed all 20 schedules.
With `everysec` and `no` all 5 power-cut schedules lost acknowledged writes, shrunk to
`disk=cut | run C=1` and labeled by design. NATS 2.12.1's `sync_interval`
(`nats-jetstream/hydo/hydo-configs.yaml`, the same seeds) with `always` passed all 20. With the
default and with 10 s all 5 power-cut schedules lost acknowledged messages (Jepsen's 3.4),
labeled by design. Each proof took about two minutes. Reference: `hydo guide workflow`,
"Which settings break a promise"; proof: `docs/HISTORY.md` Milestone 299.

**Performance tests (M237).** With `perf:` in the manifest, each workload step is its own
workload call, and `result.json` gets `perf`: the work of the workload and of each step. The
unit on hvx is the guest's retired branches (the PMU branch clock). They are exact in every run.
On KVM and the Mac the unit is VM exits, which do not count CPU work. The comparison `hydo perf
compare A B` says each change in words. It exits 1 when the work grew by more than the threshold
(default 0.5 %). In the example `hydo/onboard/examples/perf-kv`, 1 % more handler work gave
+0.871 % in every run. A changed source with the same work moved a step by at most 0.086 %.
Guest time did not track the work. The reference is `hydo guide workflow`, "Performance tests";
the details are in `docs/HISTORY.md` Milestone 237.

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

**hydo in a team's CI, what makes a team keep it (M286, tentative number).** The rest of stage
2's list, from the history store:

- `hydo ci page --store DIR` writes one web page, `DIR/index.html`, that loads nothing. It shows
  each finding across commits and branches (first seen, fixed by replay, back again, accepted),
  a timeline per branch, and the runs with their seeds and budget. Each finding has its replay
  line, its `hydo ci accept` line, and a link to the report page that `hydo ci record --report`
  keeps in `v1/<project>/reports/`.
- `hydo ci run` writes `comment.md`, which the action posts and edits in place
  (`action/comment.sh`). It shows only what the pull request changed against its base: `new`,
  `reappeared`, `changed` and `fixed`. `hydo ci comment` makes it from a recorded `ci-run.json`.
- The `workflow_run` recorder `action/examples/hydo-record.yml` comments on and records a pull
  request from a fork, whose own token cannot write, in the line `pull/<number>`. The action's
  `recorded` input runs no VM.
- `seeds_from: history` (or `--seeds-from history`) turns a budget into the first seeds that fit,
  by the seconds per seed of the store's recorded runs. The run says which it used.

The comment, the page, and the budget ran on this Mac with the Mac engine; nothing ran on GitHub
(open item 67). The details are in `docs/HISTORY.md` Milestone 286.

**The fixes of the 0.4.0 trial (M301, tentative number).** What hydo tells a user is true again
where M297's trial found it false (`docs/design/0.4.0-user-trial.md`, "After the fixes of M301"):

- A busy element of a page ends with its document (a `doc` event of `hook.cjs`), so a walk that
  goes back while a page loads is no stuck spinner. The trial's walk problem 1 was such a false
  finding. A recording replay whose run did not fail says so in `hydo explore`'s line, and
  `hydo report` and its page show and link each problem's `replays/rec-<k>/`.
- Every user-facing place names a workload step by the schedule's number, from 0 (`@3-4` is
  "during step 3"); a count stays a count ("after 12 steps").
- `hydo ci page`'s headline counts the open findings of every line and names them, and each run
  shows its start, `ci-run.json`'s `started` (a results addition).
- `hydo explore`'s first line lists the kinds its options draw, and the class `test-failed`
  speaks of the schedule's faults when it has some.
- `hydo setup` names a pinned guest file that a release lacks and does not call the release
  damaged. `HYDO_RELEASE_DIR` gives a hydo built in a checkout a release tree of its own.
- An exploration prints a progress line every 30 s. `hydo fuzz --vm --help` is the VM's help, and
  no guide topic names a milestone.

**Explorations on many KVM runners (M304, tentative number).** `hydo explore --shard I/N`
explores the I-th of N contiguous parts of `--seeds`, with the split of `hydo ci run --shard`.
`hydo explore --merge DIR... --out DIR` makes one exploration of the parts without a VM. It
decides again what one exploration decides:

- the failures in seed order, and which failure of each class and signature is shrunk (the
  part's shrink, renumbered);
- the error kinds and their shrinks;
- the property table, and the liveness summary.

`exploration.json` gains `config.shard`, `shards` (each part's seeds, counts, host CPU, and wall
time) and `missing_seeds`, a results addition. On the scripted engine the merge of 7 parts
equals one exploration of the same 60 seeds file for file (`fake_engine`'s
`merge_equals_one_exploration`). The workflow `.github/workflows/explore.yml`
(`workflow_dispatch` and `workflow_call`) builds hydo and the image once, explores up to 20
parts on GitHub's `ubuntu-24.04` runners, and merges them. `ci/remote-explore.sh` starts it,
waits, and downloads the result. The merge compares no fingerprint across parts, because a
part's runner is an AMD or an Intel host. On GitHub's runners Redis in 8 parts gave one runner's
verdicts, signatures, and shrinks. Dragonfly in 20 parts found its snapshot finding in 9 of 60
schedules, in 31 minutes against 75 on one runner. Redpanda in 20 parts found findings 1 and 2
at the seeds where hvx found them, in 2 h 53 min. The details are in `docs/HISTORY.md`
Milestone 304.

**A kill at the moment of a console event (M305, tentative number).** The reaction action
`kill-now-<svc>` (`A=<event>[@<step>][+<ms>]:kill-now-<svc>`) kills a service before any of its
threads runs on after the event's record. `kill-<svc>` acts when the host has seen the record and
the agent has run. Before the step, the host starts a relay through the agent. It is a busybox shell at
`SCHED_FIFO` 99 that reads a FIFO bind-mounted over `/dev/console` and the console device. The
record's write wakes it in the writer's own system call. With one vCPU nothing else runs until
it has slept the delay and sent SIGKILL to the service's processes, with builtins only. A
forwarder at normal priority writes every line to the console. No engine, agent, kernel, or image
changed, so the code is the same on hvx, the Mac, and KVM. `hydo explore --reactive fault
--kill-now` draws it, with delays of 0 to 50 ms. On the Mac engine
`A=meili.swap_acked@20+14:kill-now-meilisearch` runs Meilisearch's index swap twice on every run
(finding 1), and so does `+13`. `kill-meilisearch` at the same delays passes. On hvx
`A=meili.swap_acked@30+<ms>:kill-now-meilisearch` runs the swap twice at 34 to 40 ms, and
`kill-meilisearch` at 30 to 40 ms passes.
Old schedules gave their fingerprints and control transcripts on both engines. Since M309 half
of `--kill-now`'s fault reactions, drawn from a stream of their own, kill any service, and the
schedule's `K=` or `P=` on that service is left out. On Meilisearch, which has one service, 101 of
360 schedules carried such a kill. The exploration found finding 1 blind at seed 101 and shrank
it to `A=meili.edit_acked:kill-now-meilisearch` with no other fault. The reference is MANIFEST.md
"Reactions"; the details are in `docs/HISTORY.md` Milestones 305 and 309 (open item 154).

**Statistics of an exploration (M336, tentative number).** `hydo explore` and `hydo explore
--merge` write `stats.json` beside `exploration.json` (results type `stats`, an addition). It is
the host's record of the runs, so no run, fingerprint, or verdict moves. It has four parts:

- The curve of new behaviour: the distinct fingerprints, verdicts, property outcomes, error
  kinds, and error paths after each first run in seed order. The last quarter of the runs, at
  least 10, is judged `growing` or `flat`; the fingerprints are not judged.
- The cost: the engine's seconds by role and the share that re-runs and shrinks took. It also has
  the engine's clock, the host CPU and memory, and each worker's time busy, starting, waiting, and
  idle.
- The faults drawn, with the kinds that `faults:` allows and no schedule drew.
- How many first runs fit in a budget with N workers or N shards. `seeds_from: history` counts
  with the same function.

`hydo stats [DIR] [--json] [--budget T] [--workers N] [--shards N]` prints it. `hydo report`
prints a short section, and `hydo report --html` draws one inline SVG chart per judged measure.
The Redis quickstart's seeds 1-60 ran on the Mac engine (swarm, 4 workers, 62 s). The curve was
flat over the last 15 runs: 4 verdicts, the last new one at run 13, and 2 property outcomes. Re-runs and shrinks
took 39 % of the busy time, and an hour on 4 workers fits about 3,945 first runs. Pauses, one-way
network faults and clock jumps were never drawn (`--new-faults`). The reference is `hydo guide
workflow`, "How much was explored, and whether to run more"; the details are in
`docs/HISTORY.md` Milestone 336 (open item 169).

**Documentation.** `docs/STYLE.md` is the writing standard. `sh ci/vale.sh` checks 64 documents
(`sh ci/vale.sh --list`), and `cargo test -p hydo --test it docs::` checks their links and the
facts and commands of `README.md` and the quickstarts. Release step 6a is a docs review by a
fresh reader (`docs/RELEASING.md`). Before M187 this file held the full current state; that text
is kept verbatim in `docs/HISTORY.md` Milestone 187. `docs/USE-CASES.md` (M300) maps every use
case to its status (`built`, `partly built`, or `idea`) and lists the ideas not built yet.

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
   investigated (M78). At the fingerprint epoch 4 no schedule of a 90-schedule sweep reached the
   second symptom, so the list shows the issue by its first symptom (M302). A search with timed
   kills and pauses inside a step was not run.
9. Explorer gaps. The oracle reads Kafka before its restart after a kill at the last order (M55).
   The `xt` shrinker keeps a seeded policy (M49). An OK verdict shows only the first invariant's
   details (M47).
10. etcd 3.6.8 against 3.6.9 needs timed pauses aimed at the leader.
11. Images built before M71 drop netem's queue when a delay fault ends.
12. Processor Trace limits: no guest modules, no JIT-compiled Java, and buffers of about 1 GiB can
    fail to allocate (M40, M44).
13. Loops without VM exits (M48, M53, M57, M194, M274). The KVM engine interrupts one with
    `guest.branch_tick` on an Intel host with a counter of the guest's branches (M274). The Mac
    engine resolves a loop that repeats the whole machine, TigerBeetle's since M274. It cannot
    interrupt a loop whose memory changes in every pass, such as kine's: an M2 has no counter of
    the guest. The guest PMU that macOS 15 emulates with guest EL2 (M3 and later) was not
    measured.
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
38. NATS JetStream (M141, M235, M245). On hvx, one VM reaches Jepsen's 3.2, 3.3 and 3.5
    (`examples/nats-jetstream/storage/` and `blocks/`); the Mac runs of 3.1 and 3.4 have no
    x86_64 run. What is left:
    - Five nodes, the shape of most of Jepsen's runs, were not run.
    - The Mac engine and KVM did not run `blocks/` or the strict `L=`.
    - Under the strict `L=` the first schedule of 3.5 passes on the default image, because the
      stream stalls until the node with the messages is back. Which part of the strict form
      decides that (the writeback or the silent links) was not separated.
    - Seed 2 of the exploration on `blocks/` lost 4 acknowledged messages after 4 flipped bits in
      a middle block of the running stream leader, with the replicas in agreement. Not
      investigated.
39. [user decision] `~/hydo/build/explore-binlog-ret-xr-nq-disk.img` and old benchmark image sets
    on the bare-metal machine wait for deletion (M145, M146).
41. [user decision] Public content: freeze-sweep results without the system they were measured on
    (M73, M76, M92).
42. Image records: the benchmark images lack the record; a regression's `# expect: fp=` line has
    no epoch (M155).
43. The suite's recorded values of M121's agent boot check are older than M140 (M156).
44. [user decision] License compliance: the sources archives must stay available; three SPDX
    expressions are approximations; a lawyer's review is open (M158).
45. The KVM engine with several vCPUs (M213, M219, M227). The fixed SMP kernel `b40b2711…` gave
    one fingerprint per schedule on one Intel host, a Xeon 6975P-C, and on five AMD hosts (M227). On that Intel
    host a run from an explorer worker's S1 on 2 vCPUs ended with another fingerprint than the
    fresh boot. Its verdict and transcript were the same. In the fix, the snapshotting machine goes on
    in a fresh VM with the loaded states. The fix passed run 43535344097 on eight hosts. Five were Intel (Xeon 6975P-C and Xeon
    Platinum 8573C) and three AMD (EPYC 9V74 and 7763). Each passed smp-check 13 of 13, turns-check
    7 of 7 and kvm-check 15 of 15. The engine's read-back lines appear on Intel only. They name the
    hidden attributes of unusable null segments (DS, FS), which Intel's VMX normalises in a load.
    `hydo repro` of a run's folder
    and `hydo explain` were not run on KVM with several vCPUs. `bzImage-kvm-smp` is in no
    release. On AMD, RDRAND, RDSEED and TSX cannot be made to
    fault, so a guest program that uses them without checking CPUID sees the host (M159, M161). A loop that
    counts in memory is still never proven (M194). The full-state cycles and the fingerprint
    without KVM's stale event fields are the default since M204 (h closed). Of the epoch-2
    values, only the Redis example's two schedules ran on AMD, with the Intel verdicts (M210).
46. Homebrew: no Linux formula (M168).
47. The install cache: no pruning (M162). Its Linux side fetched from GitHub in M210.
48. [user decision] The Rust test SDK is unpublished until the user decides about code-first
    tests (M160, M163).
49. `hydo diff` and `hydo explain`: deeper levels need engine additions (M165).
51. Web apps (M166, M253, M254, M284). Since M253 `browser:` runs a team's Playwright tests as
    the workload and watches the pages; M166's hand-written driver stays an example. Since M254
    the screenshots leave the guest and `screens: compare` checks them against references. Since
    M284 the Mac engine runs it, with the root on the image's disk (open item 145 has what that
    left). KVM ran it in M254 on seven hosts of GitHub's runners (Intel and AMD), with hvx's
    verdicts and one fingerprint per schedule and vendor. Left:
    - M265 explains and fixes the KVM runs that first stopped in early boot with `SPIN`. The
      spin observer's own looks slowed the 4 GiB guest's memory setup past the 10 s limit.
      The web-shop example did not run again on KVM after the fix.
    - hvx did not run M254's code, so no hvx run has screenshots in its run directory yet.
    - An exploration's report page shows each problem's run's filmstrip since M334, but not the
      other screenshots, which the explorer writes to `consoles/<tag>.screens/` beside the
      run's transcripts.
    - One pinned Playwright version (1.65.1). A team on another version needs its pin added to
      `hydo_manifest::browser::CATALOG`. Firefox and WebKit were not tried.
    - Since M258 `browser.explore` walks through the pages without tests (open item 124). Cypress
      suites do not run.
    - The watcher hooks the instrumentation that Playwright's own runner uses for traces, an
      internal interface that a new Playwright version may change.
    - An exploration of `T=` clock steps and of `P=workload` pauses against a frontend was not
      run, and neither was a frontend with its real backend through `browser:`.
52. The skill: `hydo skill update` never ran against the real GitHub API (M170).
53. The benchmark images and the browser example are not in `recorded.json` (M164, M177).
55. [user decision] The label `full-ci` does not exist on GitHub yet (M171).
56. `hydo inspect` was not run on KVM; no MCP server (M178). The diagnosis's replay into a
    recovery boot ran on the Mac, not on hvx (M192).
57. `hvx_hugeram_test 8192` is in `deploy-metal.sh` since M192, but the suite has not run with it
    yet (M179, M192).
58. `tls:` and `writable:` were not run on hvx or arm64 (M180).
60. Ready-made SQL workloads: the MySQL dialect and arm64 ran only in unit tests (M182).
61. Storage faults (M183, M235, M245). The plain `L=` keeps what Linux wrote back without a sync,
    and its peers see the node's connections closed. `L=...:strict` (M245) drops the first and
    silences the node. What is left of the strict form:
    - The writeback setting is the whole guest's, so the other services keep their unsynced
      writes in memory too, and a whole-guest cut in the same run takes more of them.
    - Dirty data beyond `vm.dirty_background_ratio` and the journal's metadata still reach the
      disk without a sync.
    - A closed socket of the dead node sends its end again after a timeout, which can reach a
      peer after the bridge port is up again. Without `network: bridge` no link is cut.
    - It ran on hvx only, and the explorer does not draw it.
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
    Stage 2 (M223) and M286 ran only on this Mac with the Mac engine. The action's `merge`,
    `recorded`, `history` and `pull-request` inputs, `comment.sh`, the matrix example of `hydo
    guide ci` and the two workflows of `action/examples/` passed actionlint and shellcheck but
    never ran on GitHub; `comment.sh` ran against a fake `gh`. What is left:
    - a trial of `action/examples/` on GitHub, with a pull request from a fork (the checklist is
      in `docs/HISTORY.md` Milestone 286);
    - the examples and the guide name hydo 0.4.0, the first release with these commands;
    - an older hydo that records into a store drops the order of its runs (`recorded`), and the
      page then orders the run files by name;
    - a recorded run's seconds per seed include its shrinks and its boot. Small runs give a high
      estimate: 2.5 s against 1.2 s in the proof;
    - proposed regression files, and one shrink per signature across shards (each shard shrinks
      its own).
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
80. CPU-limit pauses (M211, M216, M226). `P=workload@<at>~<off>:<dur>` stops the whole
    workload's process tree, and `P=workload/<thread>@...` (M226) only named threads, on hvx.
    What is left:
    - A pause of threads runs on the Mac and KVM engines since M234 (the fingerprint epoch 3:
      guest kernels with cgroups). It fails Qdrant's test on the Mac at 11 of 81 starts. On KVM
      with one vCPU it fails it at 1 of 81 starts (239 ms) on four AMD and Intel hosts. At the
      fingerprint epoch 4 it fails it at 1 of 120 starts (208 ms, M302). With two vCPUs
      no start from 390 to 460 ms failed it on KVM: 50 passed and 21 ended in ERROR. A wider
      window was not searched, and the ERROR's cause was not read.
    - On an image built before epoch 5 the timer's freeze script ends the run with an error. This
      happens when a thread it listed exits before its move into the cgroup (11 of the 81 starts
      of Qdrant's test on the Mac). It also takes the first session leader with `HYDO_FROM` in the
      order of `/proc`'s names, which can be a watcher of busybox's `timeout` (M296). From epoch
      5 the timer uses the reaction's script, which takes the largest session and leaves an
      ended thread out (M338). A probe with short-lived processes gave the ERROR at 3 of 3
      offsets on an epoch-4 image and OK on an epoch-5 image. The watcher's session was not
      shown in a run, and Qdrant's 81 starts did not run again.
    - A pause of a service's thread, and pauses that repeat in every period of a CPU limit, are
      not built.
    - The blind exploration does not find Qdrant's 1 ms windows: 0 of 300 schedules with starts
      from 0 to 5,000 ms. The search needs `--pause-min` and `--pause-max` around the moment.
    - KVM did not run a pause of the workload yet: the one-off workflow on the branch
      `tmp-workload-pause-kvm` runs the etcd example on a GitHub runner. The etcd and Qdrant
      examples are not in `hydo/onboard/recorded.json` yet.
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
    code` without the assert; no build with symbols ran. The Mac engine boots TigerBeetle's
    single replica since M274; the `tie/` schedule did not run there. Since M222 a replica that stops in a run without faults
    gets its own verdict, and the checks run; the `tie/` schedule did not run again with it.
90. One shrink per cause (M218, M225). `hydo explore --shrink-causes N` shrinks up to N failures
    of one signature, each with other fault kinds. Two causes with the same fault kinds, such as
    Lago's two kinds of clock step back, still share one shrink. The default stays 1. Open items
    87, 88, 89, 91 and 92 were closed in M225: `after: commands`, `test.first_records`, a failing
    check's output in `result.json`, `services[].job`, and `T=<at>:@<point>` with
    `clock_points:`. They ran on the Mac engine and in the scripted tests only. No run used hvx
    or KVM.
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
96. A failing `.sql` check's rows (M224). On hvx with 0.3.1 and on the Mac with this tree, every
    violation of a SQL check read `<store>: N rows where none may be`, without the rows that
    Milestone 192 appends. Each explanation of the Blnk example needed a `hydo inspect` session
    with `psql`. Since M225 a failing check's lines before its last one, the rows among them, are
    in `result.json` (`details.output`) and `hydo report`; the verdict line still lacks them.
97. Network links from `depends_on` only (M224). A service that talks to another without naming it
    in `depends_on` has no link to it in the fault space. Blnk's worker reaches PostgreSQL and
    Redis, and Blnk's Compose file names neither for it; the example names them by hand.
98. The Linux release and `network: bridge` (M224). `hydo build` of the unpacked release stops with
    `HYDO_IPROUTE2 not found at /nix/store/...` until `hydo setup` unpacks `guest/store.tar`. The
    message does not name `hydo setup`.
99. Error kinds and an operation's name (M224). A line that names a reference such as `d5_q` gets
    the template `d#_q`, so every type of operation makes its own kind. The Blnk example's
    `known-errors.yaml` groups them with regular expressions.
100. Lago's invoice twice (M225). A subscription whose cancellation is not committed yet at the
    hourly billing run gets a periodic and a termination invoice for one period. The Lago
    example's RESULTS.md, finding 8, has the cause and a tested fix. It is a Lago bug, and no
    upstream issue describes it. It reproduces with Docker when the database's answers to the
    termination are held for 16 minutes across the billing run: 3 of 3 subscriptions got two
    invoices. Without such a delay it cannot happen, because the run comes 10 minutes after the
    period ends. It is reported as lago-api issue 7164 (2026-11-14). The saved
    regression `cancel-before-billing-tick` depends on the image's bytes: on the image before it,
    the original schedule gave OK. A change to the example's files can turn it into OK, and then
    it needs a new schedule. The billing rule `final_one_invoice_per_period` still counts only
    periodic invoices.
101. `hydo repro`, run by a user on the run directory of a run that root made (`sudo hydo run`),
    ends with an error that the engine cannot start (a missing file); as root it reproduces. Found
    in 0.3.2's verification on hvx. The message should name the directory's owner (the 0.3.2
    release).
102. KVM fingerprints across CPU models (M252). On Intel the one differing part was DR6, whose
    bits 11 and 16 KVM reads back from the host's hardware. They leave the fingerprint of an
    image built at epoch 3 or later; an older image keeps its values. What is left:
    - The proof run of the fix covered a Xeon Platinum 8370C, a Xeon 6975P-C and a Xeon
      6973P-C. No run measured the i7-13700HX; its DR6 of `0xffff07f0` is inferred.
    - AMD and Intel keep separate fingerprints: the guest's FXSAVE writes MXCSR_MASK and the x87
      last-instruction pointer into its RAM differently. The guest sees both.
    - The guest still reads the host's DR6 bits if it reads DR6 itself.
    - The explorer's result cache still keys KVM runs by the CPU's vendor and model.
103. The plain report page (M228). A fresh reader's review is fixed. Every error verdict but a
    failed restart gets the title `The run stopped early`. A service's own log is not in an
    exploration's files, so the page can only name the command that shows it.
104. `hydo onboard` and the plugin `hydo` (M229). `hydo onboard` ran only on the Mac, and the
    messages tutorial did not run on Linux. 0.3.3 is the first published hydo with it. Since
    M332 the skill has hydo's version and ships in hydo's release; the old skill releases
    `skill-v1.0.0` to `skill-v2.4.0` wait for deletion after 0.5.0 is published.
105. Recorded values waiting for the next re-recording pass (the batch rule in "Safety and
    coordination rules"). Each change that moves recorded values adds a line here with its
    milestone and the values it moves; the pass removes the lines.
    - Milestone 317 raised the fingerprint epoch to 5. Every image of the list gets new bytes
      when it is built (its record says epoch 5): hvm 21, kvm 6 and hvx 43. The image services'
      images also get the new launcher: `hvm/voting` and `hvm/redis-oci` (7 runs), and
      `hvx/redis-oci`, `hvx/restate-oci`, `hvx/voting`, `hvx/voting-generated` and
      `hvx/voting-loopback` (14 runs). On hvx those 14 runs are expected to move; the 76 Mac and
      19 KVM runs are likely to move. The pass records the list at epoch 5 and fills
      docs/REBASELINE.md section 29.
    - The list's epoch-4 values are the ones of the release 0.4.1, since its branch was merged
      (docs/REBASELINE.md section 28). Its re-pins of `hydo-world`, `hydo-world-tls` and
      `hydo-grpc` moved 9 of 43 hvx images (30 of 141 runs) and 9 of 21 Mac images (36 of 76
      runs). No KVM value moved. These values describe images built at epoch 4. At epoch 5 the
      same images carry the programs of 0.4.1, so the re-pin moves nothing more there. The
      release 0.5.0 re-pins these programs again with its version, and the pass runs after that.
    - Milestone 323 makes every x86_64 image built at epoch 5 keep time with its TSC on hvx
      (`/etc/hydo/exact-tick` with `tsc`). So every hvx run of the list moves at epoch 5: all
      43 images and their runs, with a new guest clock, not only new bytes. A verdict can move
      too, because the guest's timeouts now follow the virtual TSC. The KVM images get the file
      as new bytes, which the KVM engine ignores; the Mac images do not change.
    - Milestone 331 gives every image service of an image built at epoch 5 a `/tmp` of its own
      (`oci_own_tmp`). It writes a new launcher, a new folder `/.hydo/tmp` in each image's
      tree, and `HYDO_BUGGIFY_FILE` in the service's environment. It moves the same 7 images with image
      services as Milestone 317 (`hvm/voting`, `hvm/redis-oci`, `hvx/redis-oci`,
      `hvx/restate-oci`, `hvx/voting`, `hvx/voting-generated`, `hvx/voting-loopback`; 21 runs).
      Their workloads and checks reach the services through `hydo-oci` and `/tmp/hydo`, so no
      verdict is expected to move with it (docs/REBASELINE.md section 29).
    - Milestone 338 adds three changes of epoch 5. An arm64 root keeps its hard links, so a Mac
      image whose root has a file of several paths gets other bytes. The image services'
      launchers bind `/sys` recursively, which moves the same 7 images (21 runs). A timed pause
      of threads sends the reaction's freeze script, which moves the three runs of
      `kvm/qdrant-flaky-buggy` and `kvm/qdrant-flaky-fixed` (docs/REBASELINE.md section 29).
106. The terminal in plain words (M230, M247). `hydo report <run dir>` and the line `hydo
    explore` prints before its runs are in plain words since M247. The real runs of both
    milestones used the Mac engine only.
107. The onboarding fixes of M239 ran on the Mac engine only. `hydo run --memory` counts only the
    processes alive at the end, so a run with kills gives low peaks. `tcp:` probes ran with one
    service, not with `network: bridge`.
108. What the 0.3.3 docs review left is done in M247. The replay at the end of `hydo explore`
    ran on the Mac engine only, on one disk-error schedule of the Redis example. It did not run
    on hvx or KVM, or on a problem whose service writes its own log files (`logs:`).
109. Upgrades (M238). Versions of an image service (`image:`) are refused. The explorer draws one
    `U=` per schedule and only the selector `all` for a group, and the state search does not move
    `U=`. The Mac and KVM engines ran `U=` only on the scripted agent. The grace period after
    SIGTERM is fixed at 10 s. The example is not in `hydo/onboard/recorded.json`.
110. Rule suggestion (M236). What is left:
    - Lago's `api` service exits during start on hvx (the x86_64 image: a `require` error in
      `bin/rails`, with and without `guest.cpu: v3`), so Lago's rules ran on the Mac engine only.
    - Formance Ledger's six rules were proposed but not run: the ledger's Go link hung twice
      on the shared bare-metal machine.
    - The candidates file has no JSON schema, and `rules-check.json` is not in the results
      format. `messages` and `history` candidates are not run by `hydo rules check`.
    - A planted change is a change of the stored state; a change of the workload is not built.
    - `hydo rules scan` matches words, not syntax; Lago's scan gives 4,217 lines.
    - The listmonk inconsistency (a blocklisting by bounces keeps the subscriptions) is not
      reported upstream, and no exploration ran with the Blnk or Lago rules.
111. What the River onboarding lacked (M233). `hydo onboard` built and saved one image per
    Compose service that shares one `build:`, four identical images here. A service that waits
    for a one-shot job does not get the job's own `depends_on`, so the workers had no link to
    PostgreSQL (as open item 97). A group's `primary:` that asks the database fails while a
    kill of the database at the same step holds. A failing `hydo run` on hvx prints no `hydo:
    VERDICT` line. The restart of a `K=` fault ignores the service's restart policy: a worker
    that exits during its readiness check ends the run with `restart failed`. The import copies
    a Compose healthcheck `pg_isready` without `-h`, which passes during the PostgreSQL image's
    temporary init server. The River example ran on hvx only: the Mac engine was not tried.
112. The Temporal example (M231). Runs whose `eventually_` checks wait their whole deadline
    while the server logs thousands of errors end `ERROR ... exit budget exhausted` on the Mac
    engine (`HVM_RUN_EXITS` is a developer variable). Since M328 such a run names what ran, and
    an `eventually_` check that failed 3 tries or more gives its violation instead. Since M339
    the explorer shrinks such an `exit budget exhausted` error by its signature; a check cut
    short after fewer tries stays an ERROR. The
    importer's `/etc/hosts` has no line for the guest's own hostname, which Docker always gives
    a container; Temporal's entrypoint needed `TEMPORAL_BROADCAST_ADDRESS`. A check cannot tell
    whether its run had a clock step; the example's client records the clock it saw. The
    history, matching and frontend services ran in one process, not apart.
113. Function contracts (M241, M255). What is left:
    - KVM: the proof ran only in the one-off workflow `tmp-contracts-kvm`, whose branch is
      deleted. Its fourth run (43708821939) passed every check on two Intel and two AMD models
      (docs/HISTORY.md Milestone 255). No test of CI runs contracts on a real KVM host.
    - hvx has no breakpoints for contracts: the side module is designed in
      docs/design/contracts.md, not built.
    - On KVM two functions with contracts fit in one program when they read their results
      (three otherwise), one less in a position-independent program. The engine stops in the
      process of the latest system call and in processes with recorded calls.
    - A run judges at most 100,000 calls; the report says how many it did not judge.
    - From Blnk (M260): a clause reads no map or interface value. Since M285 a clause compares
      floats, and `record` lines show maps and interfaces. A report shows only the values a
      clause compares. A string is shown to the literal's
      length, past its own end, and a value read inside `old()` lacks `old()` in its name.
      The Blnk comparison ran 24 blind schedules on the buggy build and none on the fixed build;
      30 and 20 more were planned and stopped for disk space.
114. `hydo k8s` (M243). Its own call of `helm template` and `kubectl kustomize` ran in no test:
    the proof rendered the chart on the Mac and imported the output on the bare-metal machine.
    The imports ran on hvx only, not on the Mac engine, and not on KVM. These are not mapped:
    - A Deployment gives one pod, whatever its replicas, and no load balancing.
    - There is no proxy between a Service's port and its target port.
    - The containers of a pod do not share `localhost`.
    - There is no Kubernetes API (leases, operators, custom resources).
    - The link-style `<NAME>_PORT` variables are not set.

    `docs/design/k8s-import.md` has the whole list.
115. `hydo-manifest`'s `equivalence::` fails on 134 answers since M239: its `--check` warning
    for missing `root.files` sources was never recorded. The warning's text depends on the
    machine, on which Nix paths exist. Before the answers are recorded again, the test must
    hide the host's `/nix/store` or leave the warning out of the digest (M243). The `hydo`
    test's recorded answer of `hydo build <stack example> --check` differs on the bare-metal machine
    for the same reason.
116. Emulators (M242). They ran on hvx only, with loopback networking: not on the Mac or KVM
    engines, not with `network: bridge`. stripe-mock keeps no state and sends no webhooks, and the
    SMTP proxy records connections, not messages. An emulator's state is lost at a power cut.
    vercel-labs/emulate (Stripe and Resend with state, and many other providers) needs an image
    with Node.js 24 that hydo would build and pin. The changed `hydo-world` reaches a release at
    its next re-pin; so does M329's (the proxy's `Content-Length` and whole bodies, `if_header:`,
    and `errors:` on an `http` proxy's route). M329's fix and the s3 proxy's conditional routes
    ran through `hydo-world` natively and in its tests, against the pinned rclone under Docker,
    not in a VM.
117. The clock audit (M248). It reads text, so a reading that passes through an interface with
    several implementations, a channel or a queue is not followed. Its schedules place the steps
    at fixed workload steps. Only the Valkey schedules ran; the Temporal and River schedules, and
    the audit inside a new onboarding, did not.
118. Fakes from OpenAPI descriptions (M249). Response headers (`Location`), nested echoed fields
    and OpenAPI 3.1 `webhooks:` are not generated, and the business reference is not inferred
    (`--ref` names it). A `DELETE` does not remove the saved object, and a `PATCH` saves nothing.
    The example ran on the Mac engine only. It did not run on hvx or on KVM. The changed
    `hydo-world` (`errors:`, `{uuid}`) reaches a release at its next re-pin.
119. Kubernetes in the guest (M246, M256, M274). k3s runs on hvx, and on KVM with
    `guest.branch_tick` and 2 vCPUs on a Xeon 6975P-C runner (M274). What is left:
    - On the Mac engine k3s does not start. kine's watch goroutine loops on a closed channel
      without VM exits, and `select`'s random order changes memory in every pass. The Mac has no
      counter of the guest's branches (open item 13). With embedded etcd the Mac boot stops in
      another such loop.
    - On KVM the forward clock steps `T=2:30` and `T=3:30` gave a second leader span but not
      hvx's two leaders at once; other steps and the explorer were not run there. The
      `containers/` example binds a `/proc/cpuinfo` with a `cpu MHz` line for the kubelet,
      which the KVM guest kernel does not print.
    - On KVM with one vCPU k3s refuses to start without the cpuset controller, which needs an
      SMP kernel. The engine boots the SMP kernel only with `--vcpus 2` or more.
    - On the Mac engine runc needs `--no-pivot`, because the root is the initramfs; containerd
      would need its `no_pivot` option.
    - hydo has no `guest.modules:`: the example stages the modules with `root.files:` from the
      nixpkgs store path, and the Linux release ships 14 of the 57.
    - hydo's faults name services, and k3s is the only service. A kill of one pod is the
      example's knob `Z=`, not a hydo fault. One node only; operators from images were not tried.
    - The explorations of the example ran 48 schedules; the one with `--new-faults` was stopped
      after 8 runs, before it shrank its failure, on a loaded machine. Each cluster is one minute
      old, so every step back of more than an hour meets k3s's certificates.
120. The hvx engine's host-call port (M245). In `hydo explore` on the NATS example's `blocks/`
    image each of the two workers logged `HVX_HC_PUT: No space left on device` 46 times, and the
    runs still finished. `hydo run` of the same image logged none. Not investigated. In M292 a
    recovery boot of `hydo fuzz --vm` with about 90 KiB of carried ids logged it in a loop and
    did not start; its first boot had finished the workload.

121. What the Hatchet onboarding lacked (M250). A service without `image:` cannot have its own
    `env:`. YAML anchors in `hydo.yaml` are read since M316. The import
    took a gRPC server's port and readiness check from the image's `EXPOSE`. It ran one-shot jobs
    in the `setup:` of several services that start together. Links through one-shot jobs are lost
    (as items 97 and 111). `fetch-images.sh` stops on a machine without Docker for `build:`
    services. `hydo onboard` writes `AGENTS.md` and `.gitignore` into the project. A run's verdict
    names one broken rule. The example ran on hvx only: the Mac engine and KVM were not tried.
    In the exploration's seed 1, a worker did not start in the recovery boot after a backward
    clock step; its log was not read. The faults of seed 2 were not separated.
122. What the Saleor onboarding lacked (M251). None of it is fixed:
    - `hydo rules scan` reads a model file only in a folder named `models`, so it reads no
      Django or SQLAlchemy `models.py`. On Saleor it read 1,109 lines of old migrations and none
      of the 25 model files.
    - The scan reads instruction files for coding agents (`AGENTS.md`, `CLAUDE.md`,
      `.claude/skills/`) as documentation, and a GraphQL schema's names but not its
      descriptions.
    - `hydo compose` gives a worker that runs an image with another command the image's
      `EXPOSE` port and a TCP readiness check that never passes.
    - An image service's root is read-only in the guest and writable in Docker. A program that
      writes into its working directory fails quietly. Celery's beat stopped at start, and the
      service counted as ready. Neither the import nor `hydo build --check` warns.
    - A failing `first_` command's own output is not in the diagnosis.
    - An engine that the host's out-of-memory killer ends is reported with the words "the
      engine exited." `hydo rules check` then reports every rule `unevaluated` without saying
      why.
    - `hydo rules check` gives no result for a command rule that is a `hydo-world check` line,
      although the check runs and holds; the cause was not found.
    - Saleor's example ran on hvx only. It did not run on the Mac engine or on KVM.
123. What 0.3.4's docs review left (the 0.3.4 release). A `hydo ci accept` key names the failing
    check, not the cause, so two problems that fail one check share a key. The report says so.
    A key per cause would change the key format of `hydo ci` and of every
    `hydo/known-findings.yaml`. A problem whose runs failed in two ways can show one run's
    schedule in its summary and the other's in its web page. The run page's "Why it failed" shows
    the process table and the kernel log for a failed check too. The web page's property table can
    list one check twice. Without the udev rule, each hvx run leaves root-owned agent logs in
    `hydo-out/`.
124. Walks through the pages (M258, M264). What is left:
    - The Mac engine runs the walks since M284; Vikunja's walks did not run there.
    - Only one step's video is recorded: the step that a finding names (M267), else the last
      that takes an action. Since M334 every run keeps a small screenshot of every action, and a
      failing run the trace of every step, up to 5 MiB per boot. The videos of every step of a
      Memos walk would pass 2 MB.
    - A fault still changes the page itself (open item 129). A clock step changes a memo list's
      order and what a calendar shows, so a shrink candidate without it can take other actions.
    - `repro.spec.ts` maps `H=` faults to `page.route`, but applies a fault with `p=` to every
      request, and gives a backend fault only as a comment. Its native run was tried on the
      web-shop example only.
    - `hydo rules check` runs walks (`I=1`, `I=2` and on) for a walking image; it did not run on
      one. Since M334 `browser.read_back` checks that the pages show the acknowledged objects;
      it does not check their content (open item 168).
    - Only the page invariants' signatures blank generated ids (M264); a project's own rule
      whose message names a random id still gives each shrink candidate its own signature.
    - After a power cut the page checks run in the recovery boot, where no page ran; the pages of
      the first boot up to the cut are not judged. Since M334 their filmstrip and the last
      step's trace leave the guest before the cut, and the read-back check opens the pages in
      the recovery boot.
    - The walk follows no links into other origins and no file uploads, and it sees no canvas,
      no iframe, and no shadow DOM that the accessibility tree does not show.
125. What devices need from hydo (M259; the section "What hydo lacked" of
    `hydo/onboard/examples/rauc/RESULTS.md` has the details). M263 built a reboot of the guest
    (`guest.reboot`) and added disks with caches of their own (`guest.disks`); open item 128 has
    what they left. What is left of the rest:
    - The device's kernel command line, where RAUC reads the booted slot.
    - A power cut inside a step on the Mac. `cf=` runs on hvx only, so the examples cut with
      their own console records and reactions (`A=...:cut`).
    - squashfs and the device mapper in the Mac's guest kernel. RAUC needs a patch there, and
      no engine ran `verity` bundles.
    - A way to keep `G=` off a file of a service's storage, rules for system software in
      `hydo rules scan`, and disk faults and the strict `L=` in the explorer's draws.
126. The device examples (M259). What is left:
    - SWUpdate's candidate is not reported upstream.
    - The Mac engine ran SWUpdate and RAUC with M263's client (real reboots) in directed runs and
      one short exploration only, not in M259's explorations.
    - libubootenv 0.3.8, which both tools use, ignores its `fsync` result. It is fixed on its
      `master` (`ed3fce96`) and in no release; no run showed it.

    Its two hydo errors are fixed in M261: the agent's refused line and the hang after a `cf=`
    cut. What is left of the second is open item 127.
127. A `cf=` cut inside the agent's own disk operation (M261, M271). Since M271 a `cf=` run
    makes the storage faults' mounts and byte reads with busybox in a child process, so a flush
    under an `L=`'s mount leaves only the child waiting. Seed 37 of RAUC's exploration, `cf=83 |
    run T=12:-300 L=device@6+2`, reaches its recovery boot on hvx. What is left:
    - A run without `cf=` keeps the agent's own `mount` and `read`, so a `cf=` run and the same
      run without it send other commands.
    - A `G=` flip's read in a child process ran only in the scripted tests, not on hvx.
    - Any other agent command that waits on the dark disk still ends the run in the ERROR of
      M261 after 200,000 stops. A module flag that fails the dark disk's requests with an I/O
      error would end every such wait. It is a module change, and the reload is the user's.

    Also left: the agent reads its last 1 to 3 staged bytes with a 4-byte `IN`, and the hvx
    engine pads around it (M261). An agent that read them with 1-byte `IN`s needs no padding, but
    a new agent moves the agent pin.

128. Reboots and added disks (M263). What is left:
    - The hvx engine has no added disks, because its one block device is in the kernel module.
      A second device needs its own PCI function, queue, and cache. It also needs its part of
      the snapshot and of the fingerprint, and a disk window. That is a change on a side module
      first, and a reload of the main module after the suite.
    - Some runs refuse a reboot, because their state is not carried into the new boot yet:
      - before any VM: `cf=`, `U=`, test compositions, `perf:`, `fakes:`, `proxies:`,
        `emulators:`, and `walk:`;
      - at the reboot on hvx: tick jitter, extra ticks, `pp=`, and `q=`;
      - at the reboot on the Mac and KVM: tick jitter, `pct=`, several vCPUs, and `spin=rewind`.
    - hydo does not watch `reboot(2)`: a program prints `HYDO-REBOOT` instead, because a change of
      the agent changes every image's bytes.
    - A request is taken in a step, the retry, a failed start after a reboot or a cut, and a
      recovery boot before its checks. Elsewhere it is logged and ignored.
    - `G=` and `L=` do not reach the added disks, the disks start empty (no `image:`), and the
      schedule's `disk=` faults apply to the main disk only.
    - `hydo repro --transcript` types the run's transcript again but runs the boots after a
      reboot by the schedule's policy.
    - RAUC's example kept its loop devices, and RAUC did not run on KVM. Its build makes new
      signing keys and signed bundles, so a fresh build gives another image; `make-bundles.sh`
      keeps them once made. Milestone 266 found that SWUpdate's bundles carried the build's time,
      which split its KVM values between runners.
129. Walks after Milestone 264 (M267). What is left:
    - A fault changes the page itself, and so the walk. Without a clock step, five Memos walks
      take 7.4 of 72 actions otherwise on M267's walker, and 19.8 on M264's. One of the five
      takes 32 otherwise. A walk that follows the page, such as one that chooses among the memos
      by their text, is not built.
    - M267 measured why M264's walker reached neither Memos finding in seeds 21-60. The share
      link is four controls deep, behind a menu and a dialog, and M258 reached it in 1 walk of
      40. Open item 131 has what is left.
    - The shrink kept 31 of 72 actions. Halves of a step and single actions in the middle lose
      the failure when they change the page that a later action of the failing step needs.
      A delta debugging that also tries the failing step's own actions one by one is not built.
    - The KVM run used the code before the final walker (the kinds by rendezvous, the recording
      of the last step with an action, the shrink budget in runs). web-shop's seed 8 walk passes
      on KVM and fails on hvx: its double order needs a moment of the exit clock.
    - An image built before M264 ignores the range `x<step>.<first>-<last>` and walks the
      actions it was meant to leave out.
131. Walks after Milestone 267. What is left:
    - A recording shows the failing run since M270. Open item 134 has what is left of it.
    - The final walker of M267 reaches neither Memos finding in seeds 21-60. Over those 40 walks
      it clicked Share in 7 and opened Manage share links in 1; M258's walker clicked Share in
      24 walks. A dropped clock step moves 7.4 of 72 actions on M267's walker, 19.8 on M264's.
    - Breadth against a stable shrink. A version of M267 carried the family counts across
      steps, as M258 did. It clicked Share in 21 walks and a copy button in 2. But a dropped
      clock step moved 37.4 of 72 actions on it. A breadth that depends on the page alone is
      not built.
    - Faults inside a step. A kill, a pause or a network fault starts at a workload step, when
      the walk's page loads, so it never meets an action in the middle of a step. `hydo explore`
      draws a timed kill (`X=`) only for a manifest without `faults:`. Finding 2 needs a refused
      write in the middle of a step, so no schedule of seeds 21-60 with `--durable-only` can
      fail it.
    - A walk that a power cut ends is not judged by the five page checks. Two walks of that
      version clicked a share link's copy button and passed, because `C=5` cut the power (open
      item 124). Since M334 the filmstrip of the first boot shows what such a walk did.
    - M267's walker did not run on KVM.
132. What Milestone 268 left. The saved regressions are out of the image since M269 (the
    fingerprint epoch 4): `hydo build` writes them beside it, and an edited or new regression
    leaves the image's bytes alone. What is left:
    - d11 of the bug benchmark is not found since M261. It needs a service that acknowledges
      after the disk stopped completing flushes, and a `cf=` run now stops at the flush. None of
      the 61 points `cf=245..305` fails. A trigger with another fault was not searched, for
      example a power loss of MySQL alone (`L=mysql@<step>+<down>`).
134. What Milestone 270 left (the recording that repeats the failing run):
    - Memos's share-link finding has no recording yet. A walker that prints its actions changes
      the image, and with it the walks. On the first walker with M270's change, r31's and seed
      42's walks no longer open the share link's dialog, and they pass with and without `rec`.
      Since M334 a failing run keeps its own frames and traces, so no second run is needed, but
      no Memos image with M334's walker was built or explored.
    - `hydo run` and `hydo repro` of a schedule with `rec` run the schedule twice: once without
      `rec` for the actions, once recorded. A run directory's `walk-script.txt` is written but
      not read back by `hydo repro`.
    - A replayed action waits up to 10 s for its control, and an action that the run did not
      finish is tried again the same way. A recording whose pages answer more than 10 s later
      leaves actions out and says so; none of the seven recordings did.
    - The recordings ran on hvx only, not on KVM.
136. The Mender example (M272). What is left:
    - Its two Mender candidates and the mender-artifact one are not reported upstream.
    - The explorer draws a reaction's delay from six fixed values, so a window of a millisecond
      after an event is reached only by a sweep by hand. A sweep of one reaction's delay is not
      built.
    - Reactions act only during workload steps, and `kill-<svc>` keeps the service down until
      the heal. So no reaction cuts the rollback that a device runs while it boots, and the
      operator's retry after a kill comes only at the workload's end.
    - A service's own output stays in the guest for an OK run; the example writes its log lines
      to the console as well.
    - The Mac engine's guest kernel has no `/dev/loop4`.
    - hvx ran `hydo.yaml` and the four regressions since M276, each with one fingerprint over
      two runs. The two commit regressions keep the Mac's delay of 7 ms. On hvx that cut comes
      before U-Boot's environment is written, so they show the commit that exits 0 after
      U-Boot's own rollback. The rollback of a permanent update needs `+69` to `+87` on hvx
      with the first build's tools. Why `+64` gives it too and `+65` to `+68` do not was not
      investigated. No exploration ran on hvx, and KVM did not run the example.
    - The x86_64 tools build on a Mac under QEMU, whose compilers crash at random. A retry of
      each compile command makes the build finish, but a crash inside one of CMake's checks
      turns a libarchive feature off. So two builds gave two `mender-update` programs, and with
      them other fingerprints and a window of finding 2 that is 5 ms earlier. A build on an
      x86_64 host with Docker, or CMake's checks done once and pinned, is left.
    - Managed mode (`mender-update daemon`) shares the Artifact parser of the first candidate
      and did not run.
137. Walks on Vikunja (M273). What is left:
    - When the session ends, the walk does not sign in again: `setup` runs only in a step
      without a saved session. Under disk errors seed 9's walk was on the login page from step 4
      on; why the session ended was not traced.
    - A recording replay of a walk that a power cut ends records no video: the video stays in the
      first boot's guest. Since M334 the run's filmstrip and its last step's trace leave the guest
      before the cut; `disk=none | run C=5 I=1` kept 66 frames and the trace of step 4.
    - Under `disk=ioerr` a rule that copies the database through the same disk cannot read it,
      and the run ends in ERROR (seeds 1 and 8). Which read failed was not traced.
    - The words name ids of a JSON answer only. A write whose answer has no id, such as
      `DELETE`, gives `<METHOD>:<path>`, and a rule reads the id from the path.
    - No walk marked a task done. Vikunja's done check boxes are hidden inputs under a styled
      label, and they take no trial click, so the walk leaves them out. On this Mac none of the
      three on the home page took one. Drag and drop is no action either, so no walk moved a
      task between Kanban buckets. The Kanban rule judged only tasks that are not done.
138. The branch tick (M274, `docs/design/exit-free-loops.md`). What is left:
    - It ran on the Xeon 6975P-C only. The other Intel event (`0x01c4`, Skylake to Comet Lake)
      and hybrid CPUs were not measured. AMD is refused, because its exact count needs root.
    - k3s with the tick ran on one host in one run; the explorer did not run with the tick.
    - While the counter counts, every exit costs 3.8 µs more on the runners, and a read 1.7 µs.
      A landing takes about 1,500 trap-flag steps (90 ms there).
    - A fault of a stepped instruction other than #DE, #UD, #GP and #PF leaves the trap flag in
      its frame. Only user mode is counted, so a loop in the guest kernel stays the observer's.
    - The k3s example is not in `hydo/onboard/recorded.json`.
139. Function fuzzing (M275, M280, M282, M289, M292, M296, M307, M308, M318, M326, M327, M333). What
    is left:
    - The run inside the VM (`hydo fuzz --vm`, M292, M296) is a prototype that ran on hvx and
      on the Mac engine, not on KVM. What is left of it:
      - A fault inside a call comes 1 to 13 ms after its drawn delay, at the controller's next
        stop, so a window of one millisecond is reached only by chance.
      - One call per run gets a fault inside it, and the draw spreads those over all calls of
        all functions. A function of time or threads needs `--fn` and `--faults` to get enough.
      - Several vCPUs were not tried, and the thread pause names only the first thread that the
        function starts (`hydo-call#2`).
      - A `cf=` failure, or one whose input passes alone under every schedule tried, is not
        shrunk.
      - On hvx a VM's console keeps 64 KiB, so long answers past 32 KiB per boot are not read.
        A recovery boot with about 90 KiB of carried ids hung with `HVX_HC_PUT: No space left
        on device` (as open item 120).
      - The rs-snowflake candidate (an id smaller than the one before after a backward clock
        step) is not posted.
      - The timer's freeze of `P=workload/<thread>` can take a session of busybox's `timeout`
        watcher instead of the workload's, and fails on a thread that ended (open item 80). The
        reaction's freeze of M296 fixes both for itself only.
    - Rust only, natively. Go, `char`, maps and generic structures (httparse's `Header`) are not
      fuzzed; enumerations are since M307. Floating-point values are since M289, but a clause cannot name a NaN or an infinity, and the shrink of a
      floating-point value only halves it.
    - A `build` line's constructor must return the value: one that returns an `Option`
      (chrono's `NaiveDate::from_ymd_opt`) is not used, so its panicking twin and `input` lines
      stand in. A built value is read only through its `read` line, and `requires` reads only
      the constructor's arguments.
    - A `with Self` line names one type per line. Since M318 an associated type (`Self::Owned`,
      `Self::Output`) resolves when the crate's `impl` names it, or through `ToOwned`.
    - The language has one quantifier, `sorted(v)` (M307). "Every element is unique" or "every
      element keeps a rule" cannot be stated.
    - The reading is textual. Since M318 a name of two modules resolves by the function's
      module, its file's `use` lines, or a path, and since M326 through glob imports and
      re-exports. `--list` cannot tell a build failure in advance; since M326 the build's
      report names the contract line that caused it.
    - A list's shrink keeps a length that the bug needs: memchr's input kept 80 bytes. A panic
      inside code that a macro generates is still placed at the harness's line.
    - A third evaluation by a fresh user (M308, `docs/design/function-fuzzing-evaluation-3.md`)
      on six other crates reproduced 9 known upstream bugs and found none new. M318 fixed the
      first six items of its list and M326 the other two and the smaller frictions; open item
      163 has what is left of it.
    - A fourth evaluation by a fresh user (M327, `docs/design/function-fuzzing-evaluation-4.md`)
      on six other crates found no new candidate. It reproduced textwrap's `wrap_columns` panic
      (pull request 647), textwrap's `dedent` (pull request 654), and ordered-float's issue 143.
      Nothing was posted. M333 made the fixes it calls for; open item 163 has what is left.
    - Qdrant's `decompress_sequence` accepts a header of more than 64 bits per value. What
      `decode_untrusted_bytes` returns for such a blob in a release build was not checked, and
      nothing was posted.
    - The two candidates of M280 were not posted: num-integer's `i64::MIN.nth_root(1)` panics
      in a debug build, and byteorder's `ReadBytesExt::read_uint` panics for `nbytes` of 0 or
      more than 8 without a documented panic.
    - A second evaluation on six other crates (M288,
      `docs/design/function-fuzzing-evaluation-2.md`) found three candidates and two gaps in
      documentation, none posted. M289 fixed its first two problems and the smaller ones of
      `like` and of the inputs that a run shows. What is left of it:
      - A workspace member without a lock file cannot be built apart, so a failing copy's
        message cannot name the cause there.
      - A panic inside a macro-generated implementation is placed at the macro's call.
140. The SurrealDB example (M277). What is left:
    - The SurrealKV candidate is reported as surrealdb/surrealkv#536 (2026-11-17).
    - An exploration cannot leave out one fault kind, and `hydo explore` reads `faults:` without
      the build's parameters. The example explores without clock steps through a copy of
      `hydo.yaml` (`explore-noclock.yaml`).
    - No way to keep a known bug out of a blind exploration: half of the drawn clock steps go
      back, and each reached a known finding. The example turns the changefeeds off instead.
    - The explorer shrank every error of detected corruption. On hvx seed 3's shrink then wrote
      nothing for 25 minutes, and the exploration was stopped before its report. Since M339 such
      an error and its error kinds are shrunk only with `--shrink-detected-damage`; no hvx
      exploration of the example ran with it.
    - A failed check gives no service log when no service exited.
    - One node only: a cluster needs TiKV, which 4.0 moved to its own repository. `upgrade/` ran
      on hvx only. Of its power cuts inside the 4.0.1 open, which ones hit the conversion itself
      and which the data migrations after it was not separated.
141. The Dragonfly example (M278). What is left:
    - The snapshot candidate is reported as dragonflydb/dragonfly#8826 (2026-11-17). It was not
      reproduced with a real clock step outside hydo; `native/` shows the choice by modification
      time with `touch`.
    - A restarted service's earlier log (`<svc>.log.old`) is not copied into the run directory.
    - `guest.cpu` cannot be a parameter: `{{cpu}}` passes `hydo build --check`, and the
      explorer's corpus test refuses it. The example is for x86_64 only.
    - An exploration cannot leave a fault kind out. The takeover search used a copy of the
      manifest with `faults: [kill, pause, network, oneway]`.
    - The experimental AOF (`--aof`), cluster mode, and `REPLTAKEOVER` together with clock
      steps or power cuts did not run. The Mac engine and KVM did not run the example.
    - The example is not in `hydo/onboard/recorded.json`.
142. The Redpanda example (M279). What is left:
    - Findings 3 and 4 are reported upstream as redpanda-data/community#395 and #396
      (2026-11-17; the redpanda-data/redpanda repository is archived, and its issues moved
      there). A Redpanda engineer confirmed both on 2026-11-18: #395 is fixed for v26.3.1, and
      a fix of #396 is in progress. Findings 1 and 2 are not reported.
    - Finding 4 (a produce answer with an earlier transaction's offset) ran on KVM only, from
      the one-off workflow of the branch `tmp-redpanda-kvm`. hvx did not run its schedule, and it
      is no saved regression. The example's client writes one record per partition in a
      transaction, so it shows the wrong offset but not the lost record of `native/stale-offset.sh`.
    - Finding 3's cause is found and reproduces under Docker. Why the hvx runs without faults
      pass was not traced; the kill moves the partitions' leaders apart.
    - The hanging transaction came from two blind schedules, each reproducing only on the image
      it was found on (`hydo explore --schedule`, `repro --transcript`). A directed schedule that
      fails the coordinator between `begin_tx` and `add_partitions` is not built, so it has no
      saved regression.
    - Since M339 the explorer shrinks a node that does not start again and a workload that does
      not end (`TIMEOUT workload`) by the error's signature. No Redpanda exploration ran with it.
      A candidate of a `TIMEOUT` runs to the timeout, so on hvx a shrink of 40 candidates can
      take 40 times the workload's timeout; `--no-shrink` turns it off. Other errors, such as a
      drain's own error line, are not shrunk, and only the Mac engine and the scripted engine ran
      the error shrinks.
    - A run of `hydo run` from a fresh boot and the explorer's run of the same schedule differ, so
      an explorer failure is replayed with `hydo explore --schedule` or `hydo repro --transcript`.
    - `G=` chooses the damaged file and its place from the seed and the run's files; a byte or a
      batch cannot be named.
    - The `final:` read-back's output stays in the guest; the client writes its reason to the
      console.
    - `hydo rules scan --kind clock` found no place in Redpanda's C++, although the transaction
      coordinator times expiries with Seastar's `lowres_system_clock` (the wall clock).
    - Throughput: one 4,608 MiB worker, 4 to 20 schedules an hour. The blind explorations ran
      37 schedules, fewer than the 60 to 100 planned.
    - The example's topic keeps the default `retention.ms` of 7 days, so a forward clock step
      of a year deletes records before the drain and breaks `fan-out` (seed 25). Creating the
      topic with `retention.ms=-1` would keep the check strict.
    - The Mac engine did not run the example; the Mac had too little free disk. KVM ran it only
      in the one-off workflow of finding 4.
133. What Milestone 269 left.
    - A `test.regressions` entry written in hydo.yaml is part of the manifest, which the image
      carries as `/etc/hydo/hydo.yaml`, so it still changes the image's bytes. Only the folder
      `regressions/` is out of the image. Moving inline entries out needs a manifest that the
      image carries without them.
    - `hydo ci`'s image input key still covers the folder `regressions/`, because the cached
      image's folder holds `<image>.regressions.json`. An edited regression therefore builds the
      image again, with the same bytes.
    - An image copied without its `<image>.regressions.json` has no saved regressions, and
      `hydo repro --regressions` says so. The Saleor gift card regression can take an `# expect:
      fp=` line after the re-recording pass.
144. The cgroup2 mount at boot (M283). What is left:
    - KVM ran the probe once (run 44066203730, four Intel hosts). Every check passed except two
      that expected the Mac's exact sleep length. The fixed workflow
      `.github/workflows/tmp-cgroup2-kvm.yml` (branch `tmp-cgroup2-kvm`) also runs the epoch-3
      pause under 0.3.4. Its run is still to come, and no AMD host ran the probe yet.
    - An image service's root of an image built before epoch 5 gets `/sys` by a bind that is not
      recursive, so its programs read the `0::/` line but find no files under `/sys/fs/cgroup`.
      From epoch 5 the bind is recursive (M338), which ran on the Mac engine only.
    - hydo sets no limits. Every process is in the root cgroup, so a program that sizes itself
      from `memory.max` or `cpu.max` uses the whole guest. Limits per service, as Compose's
      `mem_limit` and `cpus` give a container, are not built.
    - The Mac engine and KVM did not run the example; the Mac had too little free disk.
145. Web apps on the Mac engine (M284). What is left:
    - While the helper VM runs, a build needs free disk of about three times the root: 7 GB
      for Memos. Each image of `browser:` takes about 3.4 GB. Each example fetches its own copy
      of the 1 GB Playwright archive into its `browser/`.
    - Since M290 and M294 an epoch-4 image's fingerprint hashes the disk and RAM by trees of
      4 KiB hashes. web-shop's run takes 5.7 to 5.9 s in the engine instead of 42 to 55 s.
      The RAM part takes about 43 ms of the 4 GiB, on half of the cores. The Mac engine hashes
      every page at each fingerprint, because `Hypervisor.framework` has no dirty-page log (M294).
      An image built before epoch 4 keeps the byte-wise hash of its disk. That takes 12 to 13 s
      per fingerprint for a disk of 3.4 GB, and about 1 s for the RAM.
    - A Linux host cannot build such an arm64 image, because the helper VM needs the Mac
      engine. No path with `mke2fs -d` was built.
    - A disk fault can now hit a read of the root, as on the x86_64 engines. No exploration
      with disk faults ran against a frontend on the Mac, and Vikunja did not run there.
    - A seed's walk differs between engines. Seeds 8 and 12 of web-shop's walk failed on hvx and
      passed on the Mac, whose pages answer at other moments of the guest's clock.
    - The Mac engine ran no `screens: compare` run, and its screenshots were not compared with
      those of hvx or KVM.
    - The Mac engine did not run M334's filmstrip, its copy at a power cut, or the read-back
      check.
146. Value history (M285, M291, `docs/design/value-history.md`). What is left:
    - KVM: the engine makes the reads, but no KVM run was made. Blnk's plan needs 3 entry
      breakpoints and 1 for returns, and KVM has 3 debug registers.
    - hvx has no breakpoints; the side module of `docs/design/contracts.md` is designed only.
    - A function the compiler inlined everywhere has no address to stop at. Blnk's
      `restoreTransactionFlagsFromMetadata` was recorded through its callee instead.
    - A Go map of more than 8 entries is shown as unreadable; a map lookup by hash needs the
      runtime's seed. A struct as a map's value, and an interface's value of a type outside the
      11 read types, are not shown.
    - A call through a function value or an interface method names its return address only.
    - Since M291 a `// hydo: keep` line judges a value along each key's history, and a broken
      one is a `values` finding. A property compares one value with an earlier value of the
      same value's name. A property over two names (the limit with its metadata entry), a
      comparison with a constant, and an order of calls are not built. The property table
      of `result.json` does not list the `keep` lines, and `hydo rules check` does not run
      them. No run with a power cut judged `across boots`, which is tested on synthetic
      records only. An exploration's plain problem text (`hydo report <explore dir>`) does not
      quote the broken key's history; its `report.md` does.
    - The view's step comes from the transcript's `workload steps a..z` notes, so a run
      without faults reads `workload steps 0..29` for every stop.
    - A run with a power cut was not recorded; the boot number of the records is tested on
      synthetic records only.
147. The TiKV example (M287). What is left:
    - Finding 1 (crashed transactions' locks after a backward clock step) is the known and accepted
      pingcap/tidb#44822, so it is not reported. Another way to reach it was not run: a move of PD's leadership away from a
      member whose clock runs ahead. Pessimistic locks and async commit's locks under the step
      were not tried.
    - Seed 4 of the first exploration also timed out the readers of the locked accounts. The
      directed runs blocked writers only, and why that run's reads could not pass the lock was
      not traced.
    - One explorer worker of a 7,680 MiB guest uses about 19 GB of the host. So the explorations
      ran on one worker. A `hydo run` beside the explorer ends with `the engine exited` and no
      reason.
    - After a kill of a group, hydo restarts its members one at a time, each after the one
      before is ready. So a member whose readiness needs a quorum never becomes ready. In the
      example, PD's members count as ready at once. So a PD member that dies in the recovery
      boot is not named, and the verdict names the store that cannot reach PD (finding 2).
    - Finding 2 (tikv/pd#11596) is known and fixed on PD's master only. A power cut shortly
      after any start of PD reaches it, so explorations with power cuts meet it often until a
      release has the fix.
    - TiKV's address lock in `/tmp` was found by the other stores. M331 traced it: every image
      service had the guest's own `/tmp`, with `root.share_images` and without it. From the
      fingerprint epoch 5 each image service has its own, and `hydo compose` writes
      `share_images: true` again. The example was not run again at epoch 5: its stores still
      listen on their own addresses, which needs no change.
    - TiKV's gRPC client did not reach PD by service name in the guest; the cause was not
      traced. The example names PD and the stores by address.
    - `hydo rules scan --kind clock` does not see a timestamp compared with a time to live
      (`lock.ts.physical() + lock.ttl`). Since M324 the test it ranked fifth is left out; the
      cloud-credential code stays first, because `tikv-server` links it.
    - TiKV logged `monotonic time jumped back, 52.048 -> 45.589` once on tikv1 in seed 4's run
      (the receive delay of a Raft message); whether the guest's clocks or TiKV's comparison
      caused it was not traced.
    - hydo kills services, not the workload at a point of its code, so the client crashes inside
      its commit by itself (`anytime_crash`, with client-go's test helpers).
    - The Mac engine and KVM did not run the example. The TiKV image is 806 MiB compressed, and
      each store holds about 2 GB at its start.
148. The Meilisearch example (M295). What is left:
    - Its three candidates are not reported upstream.
    - A kill reaction (`A=<event>+<ms>:kill-<svc>`) reaches the service through the guest
      agent, and kills at 18 delays never stopped Meilisearch between the swap's two commits.
      Since M305 `kill-now-meilisearch` does, at 13 and 14 ms after a swap's `202`.
    - A run's verdict line names its first broken check only. Every power cut within 30 s of a
      dump breaks `final_dumps`, and finding 1's schedule shows the swap only in `hydo report
      --details`. The example's build parameter `dumps=no` keeps finding 3 out of explorations.
    - The explorer draws a reaction's delay from fixed values. No blind schedule came between
      the swap's commits. The moment of a cut after an event moves with the image's bytes. A
      change of the client moved finding 1 from 2 ms to 40 ms after its event.
    - Released 0.3.4 (epoch 3) cuts at other moments and misses finding 1 with `+2` and `+40`.
    - The shared Mac VM lock: the onboarding waited about 1 hour 40 minutes for it.
    - The example ran on the Mac engine and, since M305, on hvx with reactions; no `cf=` run, and
      KVM did not run it.
149. Differential testing (M298, `docs/design/differential.md`). What is left:
    - It ran on the Mac engine only. hvx and KVM did not run it.
    - The outcomes come from the controller's reads of `$HYDO_OUTCOMES`. A run that ends in an
      error before the read has none, so its side shows no outcome lines.
    - A reboot's conversations (`guest.reboot`) are not read; only the run and the recovery boot.
    - A workload that chooses its next operation from the order of earlier answers chooses
      otherwise on another version (Formance's reverts). No rule can blank that; the workload
      needs to choose by a seed alone.
    - A timing change differs under network faults (Redis `hz`). There is no option that draws
      schedules without the fault kinds that only move timing.
    - The SDK assertion records of the workload (`HYDO-ASSERT` without a check's output) are not
      compared. Formance's `ledgerc.retry_acked`, only in v2.4.0, showed only in the console.
    - `hydo/compare.yaml` has no JSON schema, and `hydo ci` does not call the command.
    - A shrink keeps the first difference's stream, not its record, and runs each candidate on
      both versions one at a time.
150. Settings explored with `hydo explore --configs` (M299). What is left:
    - It ran on the Mac engine only, with two settings of one value list each. hvx and KVM, a
      manifest with several settings, and the `pairs` mode on a real project did not run; the
      pairwise set is tested on value counts only.
    - The explorer names a failure's class by its `knobs:` tokens and disk faults. So a loss
      under a relaxed build setting has the class `durable-defaults` after a power cut. Since
      M328 such a loss without a power cut has a class of its fault families (`process`,
      `network+clock`). The table's label says whether a `tradeoff:` note explains it.
    - The label is by values, not by cause: any failure found only with values that have a
      note is labeled the documented trade-off, also a crash with another cause.
    - The combinations run one after another, each with its own worker pool, and each image
      stays in the image cache, which nothing prunes (as open item 47). The NATS images are
      1.1 GB each.
    - A setting that the system changes at run time stays a `knobs:` entry (`R=`); such an
      entry and a build setting of one parameter are not merged.
    - `hydo ci run` does not run the combinations, and `hydo versions` cannot replay a finding
      on a configuration.
151. What the fixes of the 0.4.0 trial left (M301, `docs/design/0.4.0-user-trial.md`). What is
    left:
    - A recording replay has timing of its own. A failure that depends on the walk's timing can
      pass in it, and then no video shows the failure. The page says so. Since M334 the failing
      run's own filmstrip and traces show it; a video with the run's own timing is not built.
    - The images of `browser:` built after M301 have new bytes (`hook.cjs` and `check.mjs`). The
      fingerprints in the RESULTS.md pages of web-shop, Memos, and Vikunja hold only for the
      images built before. None of them is in `hydo/onboard/recorded.json`.
    - `ci-run.json`'s `started` ran in one `hydo ci run` on this Mac; nothing ran on GitHub
      (open item 67).
    - `HYDO_RELEASE_DIR` ran on the Mac with a release tree made by hand from 0.3.4's tree and
      this tree's programs, not with one that `build-release.sh` made.
    - The help of `hydo diff`, `hydo explain` and `hydo inspect` still names files under
      `docs/design/`, which a release does not ship.
    - These frictions of the trial stay:
      - `hydo ci --help` names no `hvm` engine, and the CI report's replay command has no
        `--image`.
      - The Blnk example's `RESULTS.md` fingerprints differ from a tree build's, and `hydo
        inspect values` prints `1 keys`.
      - `hydo fuzz` has no property of a function alone, and it prints an empty `fuzz.checks:`.
      - `hydo guide strategies` does not say which engine each technique needs, or how to
        choose a step for a new system.
152. The example `restate` (M303). What is left:
    - Its candidate is not reported upstream.
    - An invocation that waits in an inbox at a step back without a restart was not tested. An
      example is one queued behind a virtual object's lock. The runs without a restart passed.
    - A check's output is kept up to a size, and the verdict line is then the last line kept. A
      long output gave the verdict `VIOLATION [jobs] j25: sys_`. The violations were left out of
      `result.json`, so the example prints its long lines first.
    - One `K=` per schedule: a kill of the deployment and a kill of the server at other steps
      cannot be one schedule.
    - The clock audit reads `SystemTime::now` and its kin, not a project's own clock. Restate's
      scheduler reads `restate_clock::WallClock`, and the audit's 8 places missed it.
    - A kill or a stop reaction did not hold a plain send in flight across a restart, because a
      round's commands run in milliseconds; the explorer's pause of the deployment did.
    - Exploration B (swarm) was stopped before its shrink, and the KVM engine did not run the
      example.

153. Explorations on many KVM runners (M304, tentative number). What is left:
    - `ci/remote-explore.sh` ran once from `main` (2026-11-18): the Redis example's seeds 1-40
      in 8 parts gave 8 failures in 3 problems, as one runner does. Since M310 the script
      writes the report's commands again where the folder is downloaded, with the `hydo` on
      `PATH`; without one they keep the runner's paths.
    - Each part shrinks its own failures, so a key that several parts find is shrunk in each of
      them and only the earliest shrink is kept. The same holds for the shrinks of new error
      kinds. In Redpanda's part of seeds 27 and 28 such shrinks took most of its 9,469 s. One
      shrink per key across parts is not built.
    - Each part makes `hydo explore`'s diagnosis replays of its own first four problems, so a
      problem of the merged list can lack its replay.
    - The parts must be one engine: an hvx part and a Mac part do not merge. A failure found on
      an Intel runner replays with its fingerprint only on Intel, and the same for AMD.
    - The build job compiles hydo without a cache (about 6 minutes), and each shard downloads
      the image (Redpanda's is 3 GB). It uses the guest files of the newest release in
      `hydo/dist/releases.txt`, so a commit that needs newer guest kernels stops at their check.
    - A part's property table keeps at most 256 properties (`MAX_PROPS`); the merge applies no
      such limit.
    - On Dragonfly the first runs' fingerprints of the Xeon Platinum 8573C differed from the Xeon
      6975P-C's, at the fingerprint epoch 4 (0 of 6 seeds equal). Open item 102's proof did not
      cover that model. The cause was not looked for.
    - The workflow passes the example's `known-errors.yaml` to the parts since M310; no run on
      GitHub used it yet.
154. The kill at the moment of a console event (M305 and M309, tentative numbers). What is left:
    - The relay sees only the records of a process that opens the console after the reaction
      is armed. A record written through a file descriptor opened before reaches the host but
      not the relay, and the reaction does not fire. An SDK in a service started at boot writes
      so. A service in a mount namespace of its own does not see the bind mount either.
    - With several vCPUs the service's threads on other CPUs run until the kill's interrupt
      reaches them. No run used several vCPUs.
    - One `kill-now-` reaction per schedule, because one relay sits over the console.
    - The delay is in whole milliseconds. Meilisearch's swap window on the Mac engine is 2 ms
      wide on that grid; a window narrower than 1 ms can fall between two delays.
    - Since M309 `--kill-now` takes half of the fault reactions for a kill of any service and
      leaves out that service's `K=` and `P=`. So such a schedule never also kills or pauses
      that service by a step. A take is not tried with `cf=`. The blind exploration ran on the
      Mac engine only, with the process strategy.
    - KVM did not run it. The mechanism is the same controller code as on the Mac and hvx, with
      no engine change.
155. Error paths (M306, tentative number; `docs/design/error-paths.md`). What is left:
    - Only the Mac engine watches them. KVM's root is on a disk, so a program's pages reach guest
      RAM only when the guest reads them; the engine would search at system calls and write
      `INT3`. hvx needs the contracts' side module. hvx and KVM runs say that nothing was watched.
    - The search finds a program by the pages of its file in the guest's page cache. A program
      that reads its own file while it runs reads the breakpoint instructions; no example does.
    - In an optimized build, a Rust `?` whose error needs no conversion has no instruction of its
      own, and is no place. A `return Err` that shares its code with another return counts among
      the places without code. Rust ran only in the compiler's test, not in a VM.
    - A function named for a recovery that runs at every start (Blnk's `recoverPanic` and
      `logrusRecovery`) runs in every run that restarts the program, without an error.
    - The runs record from the workload's start, so the services' start-up is not watched.
    - `hydo explore --merge` adds up the parts' error paths since M310. It was tested on records
      only, because no sharded run watches them: only the Mac engine does. The CI page does not
      show them. A declaration that a place must run in an exploration, which fails it, is not
      built.
157. The ClickHouse example (M311, tentative number). What is left:
    - Finding 1 is known (#53143, open). A comment with its power-cut trigger was not posted.
    - The hvx engine takes at most 4 GiB of root and data disk together (`HVX_DISK_MAX`).
      ClickHouse's two programs are 1.4 GB. So the services of each image share one tree, and the
      data disk has 1,024 MiB; a third replica or a larger data set does not fit.
    - After a kill of a group, hydo restarts its members one at a time, each after the one before
      is ready (as open item 147). A Keeper opens its client port only with a quorum, so the
      Keepers count as ready at once, and a Keeper that does not start is not named.
    - `hydo compose` took the Keeper's port from the image's `EXPOSE` (2181; it listens on 9181)
      and kept the images' `entrypoint.sh`, which changes owners in the read-only root. The
      import does not propose `guest.cpu: v3`, without which the programs stop with SIGILL.
    - The four schedules that ended `TIMEOUT workload` after 900 s were the guest's clock, not
      the agent (M319). With `guest.exact_tick` seed 20 ends with a verdict and seed 33 passes.
      Seeds 27 and 40 did not run again (open item 164).
    - The busy `KeeperHandler` threads of those runs are a ClickHouse candidate (M325): a handler
      polls the socket of a closed session until the session timeout. It is not reported
      upstream, and nothing was posted. It was not reproduced outside hydo, because the cause is
      plain in the code. Seeds 27, 33 and 40 were not checked for the same loop.
    - `hydo explore --configs` with `--no-shrink` or `--schedule` lists no class of failure and
      says that no combination failed, although 6 of 24 and 2 of 3 runs failed. An exploration's
      failures are recorded only when they are shrunk.
    - A workload command's output stays in the guest (`/tmp/hydo/compose/log.<n>`); the client
      copies its errors to the console to show them in a run's `one.log`.
    - Two of the onboarding's VMs of 4 GiB beside other sessions made the host's out-of-memory
      killer end one of its runs; its scripts then held one lock for every VM.
    - Only hvx ran it: not the Mac engine (the images have arm64 builds) and not KVM. No `cf=`
      run, no third replica, and no Distributed table, materialized view, or lightweight update.

158. The example `victoriametrics` (M312, tentative number). What is left:
    - The three candidates are not reported upstream. Each cause is plain in the code, so no
      native run was made.
    - hvx's console holds 64 KiB, so the client names a sample by its write and number and
      writes 6 samples per write. Nothing warns while a run's history records are cut off.
    - The explorer shrank every error of detected corruption; the second exploration left `G=`
      out through `explore-nocorrupt.yaml`. Since M339 it shrinks them only with
      `--shrink-detected-damage`. The swarm exploration ran without shrinks.
    - The last `T=` of a schedule wins (open item 65), so no schedule steps the clock back and
      forward again. That the deleted month stays deleted is read from the code and the files.
    - The clock audit does not follow `fasttime.UnixTimestamp()` or `time.Now().UnixMilli()`
      into a deadline, so it named none of the three places.
    - Only hvx ran the example; the Mac engine and KVM did not. vmagent, several vminsert or
      vmselect nodes, three vmstorage nodes (the documentation's `2*N-1`), and the rollup cache
      of `query_range` were not tested.
159. The Iggy example (M314, tentative number). What is left:
    - The example runs one server. Iggy also replicates partitions over a cluster with its own
      consensus, and no cluster of three nodes ran under the cluster faults of `groups:`.
    - The example ran on hvx only. The Mac engine and KVM did not run it.
    - The expiry check needs the wall clock at the moment of each deletion, which no check can
      read. The client records the clock's offset from the uptime at every send and probe, and
      the check takes the latest clock that a command saw. A step forward that no command saw
      before a deletion would make a deletion on time look early; the check allows 2 s for it.
    - The expiry probe cannot tell a lost message of the expiring topic from one deleted early.
      With `replicated` it reports the loss as an early deletion.
    - Timestamp polls after a backward clock step were not tested: a partition's message
      timestamps then go down.
    - The client is on the Go SDK: the Rust SDK needs Rust 1.95, and the bare-metal machine has
      an older Rust.
    - The swarm exploration ran 24 of its 40 schedules: the host's out-of-memory killer ended one
      of its engines on the shared machine. Seeds 105-120 did not run.
160. Faults of the disk's blocks (M313, tentative number; `docs/design/block-faults.md`). What is
    left:
    - KVM ran the example's 14 schedules twice in a one-off workflow (run 44380177675). Each gave
      the Mac's verdict and one fingerprint on an AMD EPYC 7763 and two Xeon 6975P-C hosts, and
      the two Intel hosts agree. No test of CI runs the faults on a real KVM host.
    - hvx refuses them: its disk is in the kernel module. The design page lists what the module
      would need, first on a side module.
    - The added disks (`guest.disks`) have none of them, and `G=...:bad|rot` maps only ext4 files
      with extents on the data disk `/dev/vda`.
    - `mis=` and `hang=` count every request of the disk, the journal's and other services' too,
      so a project finds its `<k>` in a run's `one.block-faults.txt`. The explorer draws `<k>`
      from 1 to 200, and the example's workload makes about 24 requests, so most drawn `hang=`
      and `mis=` tokens of a short workload do nothing.
    - The explorer draws no raw `lse=`, and draws `G=...:bad|rot` only when `--storage-faults`
      drew no `G=`. A torn write needs a schedule that already has `disk=cut` and a power cut.
    - On Redis, `hang=30+1000` made Redis stop after a failed flush, and its restart stopped at
      `FATAL AOF MANIFEST FILE ERROR`; the file system's state after the failed flush was not
      traced.
    - The engine's notes of what a fault did are in a file of the run directory only; `hydo
      report` and its page do not show them.
161. `hydo update` (M315, tentative number). What is left:
    - It ships with 0.4.1. Versions 0.4.0 and older lack it, so their
      users upgrade once by hand, as the section "Upgrading" of `docs/INSTALL.md` and the skill's
      `references/install.md` say.
    - The install kinds ran only against the fake release, brew, cargo and claude programs of
      `tests/update.rs`. No real `brew upgrade`, `cargo install`, `install.sh` or `claude plugin
      update` ran through it, and nothing ran on Linux. The real `--check` ran on this Mac
      against GitHub's release list and Claude Code's real configuration folders.
    - On Linux it reinstalls the module when `hvx` is loaded, and passes `--udev` with the
      rule's group but not a side module's `--udev-name`. A module that is installed but not
      loaded counts as no module.
    - A copy made before the registry is found only in the known folders until `hydo skill
      update` runs in its project. A project install of the plugin whose project folder is gone
      is reported and skipped.
    - Homebrew installs the formula's version only, so `--version V` with Homebrew is refused
      unless V is the formula's version.
    - GitHub's rate limit (M322). The line at the limit, the exit status 4, the token, and the
      kept answer ran against the fake release server only. GitHub's limit was not reached in
      the real check. GitHub's secondary limit (a 403 with `retry-after` and requests left) is
      reported as a plain HTTP 403. A command does not fall back to an older kept answer at the
      limit. The skill's `references/install.md` does not name the exit status 4.
    - The skill in hydo's release (M332). No published release carries the skill's archive yet;
      0.5.0 is the first. Until the `skill-v` releases are deleted, hydo 0.4.1 compares the
      plugin's 0.4.1 on `main` with `skill-v2.4.0` and offers a Claude Code plugin update that
      changes nothing. That `claude plugin update` installs 0.5.0 over 2.4.0, a lower version,
      rests on Claude Code's documentation only; no run tried it.
162. The fixes of a user's onboarding with 0.4.0 (M316, M317, tentative numbers). What is left:
    - The missing `/dev/shm` of the user's recovery boot has its cause since M317. An image
      service's launcher and its `hydo-oci` helpers bound the system folders outside the lock of
      its `mounts:`. So a readiness check could bind `/dev` again over the tmpfs, and the user's
      `/proc/self/mountinfo` shows that order. On hvx a project of seven PostgreSQL image
      services gave second binds of `/tmp` and `/data` in first and recovery boots before the
      fix, and none after it. No run here gave the user's exact case, a second `/dev` over the
      tmpfs; a test forces that order. The fix is in images built at the fingerprint epoch 5
      (0.5.0), so an image built by 0.4.x keeps the race until it is built again. The user's own
      project did not run with the fix.
    - The diagnosis's environment rule reads the last log lines only. A service whose log names
      both a permission error and a damaged file keeps the claim of lost data, and a service
      that logs nothing gets no sentence.
    - The s3 proxy's empty objects had their cause in M329: the proxy dropped the service's
      `Content-Length`, so rclone read a size of 0 after each upload and deleted the object. The
      fix reaches the guest with the next re-pin of `hydo-world` (open item 116). An image
      service sees the emulators' names only when it mounts the manifest's `/etc/hosts`, as
      `hydo compose`'s services do.
    - No x86_64 image with a FIFO was built; the debugfs commands ran in a Linux container only.
    - `hydo compose`'s `guest.ram_mib: 1024` was too small for the PostgreSQL image on the Mac:
      the initramfs did not fit (`Initramfs unpacking failed: write error`), and the run failed
      later with an unrelated message. Since M328 the boot ends at that line with its own ERROR,
      which names the sizes and `guest.ram_mib`. Since M338 `hydo build` and `--check` warn and
      name 1,280 MiB, and an image's hard links stay links in an epoch-5 root. PostgreSQL's root
      still needs more than 1,024 MiB: 467 MiB beside a 142 MiB archive.
163. What the fixes of the third `hydo fuzz` evaluation left (M318 and M326, tentative numbers;
    `docs/design/function-fuzzing-evaluation-3.md`, "After Milestone 326"). What is left:
    - The cause of a test module's compile error is found by the text around the error's
      place. A `with` line is named only for an error at the call. An error that no contract
      line explains is named as hydo's own, with the output to report. The test module's text
      is formatted only when `rustfmt` is beside cargo or on PATH.
    - The language has no named predicates, so half's NaN test is still written out in each
      clause, now shorter with the bit operators. On an integer whose type hydo does not know
      (`!5`), `!` stays the negation.
    - Two lists of one length are generated only for a `len(a) == len(b)` conjunct of
      `requires`, and only for lists, not for strings.
    - A pattern names a tuple variant's fields by position only, not as `Name { 0: _ }`.
    - A name of a module selects its functions only when no function has that name; else
      `--fn <name>::` does.
    - A `&'static mut` parameter is read after the call through its raw pointer while a result
      may still hold the reference; no run showed a wrong value. In a crate that forbids
      `unsafe_code` such a function is refused.
    - The resolution follows `use` lines a few steps as text, without checking that a
      re-export is `pub`; the `use` lines of an inline module count as its file's.
    - A generated closure of a generic `impl` block's type parameter, one with a reference
      result, and a `FnMut` with state are not generated. A closure whose result hydo cannot
      generate is refused.
    - An associated type that neither the crate's `impl` blocks nor `ToOwned` names, such as an
      `Iterator`'s `Item` from the standard library, stays unread; the report names it.
    - percent-encoding still needs a helper of its own to build an `AsciiSet` from bytes; the
      refusal now names the helper, and with none it says to add one.
    - The proofs used contracts written again from the evaluation page by the one who fixed the
      tool, on the crates.io archives. The fourth evaluation (M327) was that fresh run.
    - What M333 left of the fourth evaluation (M327, the evaluation page, "After Milestone
      333"):
      - A constructor without arguments and builder methods (base64's
        `GeneralPurposeConfig::new().with_encode_padding(pad)`) is no line that hydo generates,
        so a contract that reads such an option needs a helper in the test module.
      - An `input` line holds for its own contract only. A constructor that refuses random
        arguments (base64's `Alphabet::new`) needs the line in each contract, else 1,000 of
        1,000 inputs are thrown away.
      - A `build` or `read` line holds for the type's last name, so two types of one name in two
        modules share one line.
      - An associated type resolves through the crate's `impl` blocks, or as the `Item` of a
        list or an array; others, such as `Iterator::Item` of a type of the standard library,
        stay unread.
      - A structure with a lifetime parameter or a reference field is read, not generated; a
        generic structure with a private field needs a `build` line.
      - A variant behind `#[cfg(...)]` is left out whatever the features are.
      - The two special floats come from the parameters and their fields, not from a list's
        elements. An index is put inside its list only for `<`, `<=`, `>` and `>=` with `len()`
        in `requires`.
      - `--features` does not reach `hydo fuzz --vm`.

164. The guest's clock on hvx (M319 and M323, tentative numbers; `docs/design/guest-clock.md`).
    Since M323 an x86_64 image built at epoch 5 keeps time with its TSC. What is left:
    - An image built before epoch 5 keeps the gap of 152 ns a tick. A busy guest there still
      runs its clock slowly after some minutes of uptime. Rebuilding the image at epoch 5 fixes
      it; the re-recording pass rebuilds the examples (open item 105).
    - The KVM engine's guest kernel was checked by its source only: its clocksource and its timer
      read one exit clock in nanoseconds. No KVM run measured a long busy run. The Mac engine's
      probe showed no gap.
    - The TSC clock ran on the probe, the stack of the pinned reproducers, ClickHouse seed 20,
      and an epoch-5 Redis image with `T=`, `tj=`, `xt=`, `pp=` and two vCPUs. No exploration ran
      with it, and the hvx examples' verdicts at epoch 5 come with the pass.
    - A clock read still costs the guest 2 µs, and every counted exit 1 µs. A program that reads
      the clock in a tight loop sees its time run 2 µs per read.
    - Seeds 27 and 40 of the ClickHouse explorations did not run with either clock. Seed 33 ran
      once with `clocksource=jiffies` and passed, but not without it on the same image.
    - A run whose guest clock falls far behind the virtual TSC still ends only at the engine's
      wall limit, as `TIMEOUT workload`. Neither the verdict nor the diagnosis says that the
      guest's clock fell behind.
    - The controller's `run` commands of `tc` and the agent's own `qlen` (which runs `tc` and
      waits for it) carry no deadline of their own. No run showed one of them blocking: in M319
      the agent answered every command.

165. Images in private registries (M320, tentative number). What is left:
    - No real private registry ran. The proof used a registry in the test process (the token
      flow and basic authentication) and Docker's `registry:2` with htpasswd (basic only). The
      user's registry, behind a credential helper, is still to try.
    - `registry:2` with a token service was not run; the token flow and the identity token's
      refresh ran against the test's own token service only.
    - Plain HTTP goes only to a registry on the loopback. A registry that Docker's
      `insecure-registries` names, or one with a private certificate authority, is refused.
    - A credential helper's own messages are not shown, as they may hold a secret. A helper that
      fails is named with its exit status only.
    - `--from-docker` applies to every image whose registry fails, not to chosen registries.
      `hydo k8s` and `hydo onboard` do not take it; the user runs `hydo compose` with it.
    - A pin made on a Mac has arm64 only. fetch-images.sh on Linux saves amd64 from that
      machine's Docker, which needs the image pulled there for linux/amd64.
    - `docker save` without `--platform` (Docker before 28) is tried after an `unknown flag`
      error, but no run used such a Docker.
    - `hydo build` compares no image file with the lock file, for any image. A changed file is
      caught by `hydo compose` and fetch-images.sh only.
    - hydo-build's equivalence corpus has 39 mismatches. Six are from M306: two mutants with a
      `contracts:` key the recorded answers do not list. Twelve are from M329: the known keys of
      a fake's and a proxy's route. The others name the guest keys of later milestones.
166. The three gaps of a user's onboarding with 0.4.0 (M324, tentative number). What is left:
    - Since M338 the estimate on the Mac counts the whole root and its compressed archive, and
      the build measures the staged root. The estimate takes 8 % for the files' whole pages and
      fixed sizes for hydo's own files (12 MiB, 5 MiB compressed), measured on two images. The
      x86_64 disk check still counts the image layers only.
    - `hydo compose` keys an image by its manifest digest, so two manifests of one config (one
      image pushed twice) are not proposed for sharing; hydo-build would share them.
    - `hydo compose` writes `root.share_images: true` on since M331. A manifest that it wrote
      from M324 to M331 keeps the line commented out until the user turns it on.
    - `hydo inspect diagnose <explore dir> --run` ran only through the scripted engine, and the
      exploration's own transcripts in the test were the Redis run's, copied in. No real
      exploration of an agent image was diagnosed. `--evidence` needs the run directory.
    - The web page of an exploration does not show the run directory's command.
    - The clock audit maps Cargo binaries and Go main packages only. Valkey's C, and Java,
      Python and JavaScript projects, are ranked by their words alone. A Go program is named
      after its folder, so Temporal's `temporal-server` (built from `cmd/server`) was found by
      the image's name `server` only. Cargo features, optional and target-only dependencies
      count as dependencies.
    - A place that several services run (TiKV's three stores) keeps `<service>`, with a line
      that names them. `<replica>` and `<database>` keep their roles' rules.
    - The test code of JavaScript is found by `describe(`, `it(` and `test(` at a line's start,
      so a product function named `test` called there is left out too.
167. Shared beginnings of runs (M337, research, tentative number;
    `hydo/explore/RESEARCH-shared-prefix.md`). Since M339 a repeated schedule takes the earlier
    result, and the explorer writes `dseed=0` where the seed decides nothing. What is left:
    - The answers ran on the scripted engine and on the Mac engine's Redis quickstart only,
      where a run takes 0.1 s to 0.3 s. No hvx or KVM exploration measured the saving, and
      `hydo-bench` and the state search do not use the answers.
    - The counts `repeats` and `repeats_saved_s` depend on which runs were in flight together,
      and a merge adds up its parts', which answer only their own repeats.
    - Explorations made before M339 have drawn schedules with another `dseed=`, and so other
      texts and fingerprints than a new exploration of the same seeds. Their verdicts are the
      same.
    - Shrink candidates that continue from snapshots of earlier candidates at their step ranges,
      on the Mac engine and KVM. KVM has no proof on a real host that a continued run gives its
      fingerprint from S1 (open item 85). A snapshot lives in one engine process, and a round's
      candidates run on all workers.
    - No engine command changes the disk's policy or seed at a stop, so a run with `disk=cut`
      and one with `disk=none` share nothing, although the guest sees no difference before the
      cut.
    - The measurements read 15 hvx explorations. The planned Mac explorations and the Mac's cost
      of `SNAP` and `RESUME` did not run: the shared VM lock was taken and the disk was low.
      KVM explorations were not measured. The wall seconds are estimates from the guest clock,
      which predicts a run's seconds badly on ClickHouse and Dragonfly.
168. The frames of a failing run and the read-back check (M334, tentative number). Left:
    - Only hvx ran them, on Vikunja and web-shop. The Mac and KVM engines did not run the
      filmstrip, the copy at a power cut, or the read-back check. Their larger console (32 MiB)
      ran only in the scripted tests.
    - The frames and the trace of every step cost the walk guest time, so a walk's timing
      differs from M273's walker. Vikunja's `disk=cut | run C=6 I=1` lost two tasks then and
      holds now; `disk=none | run C=5 I=1` loses three now, two then.
    - Only the video of `,rec` is a video; a failing run keeps frames, not a video of itself.
    - The copy at a power cut takes the last step's trace only. A `cf=` cut at the workload's
      end, which the engine makes after the workload, copies nothing before the cut.
    - The read-back check judges only objects whose write returned an id, on the pages it
      names. It does not see a second page of a list, a hidden or archived object, or a cascade
      of a delete without `unless`. It judges an object's presence, not its content.
      `read_back: true` guesses the page and the link, and a guess that does not fit an app
      gives a false finding, so the check is not on by default.
    - In a recovery boot the check signs in with `read_back.setup`. A walk whose account the
      power cut lost cannot sign in, and the check then fails without naming the lost account.
    - Memos was not run with the new walker, and its share-link finding still has no recording
      (open item 134).
169. Statistics of an exploration (M336, tentative number). What is left:
    - Guest time is not in the cost: the explorer has no guest clock for a run on every engine.
      The engine's clock is VM exits on the Mac and KVM and hvx's own units on hvx.
    - The host CPU and the memory are the whole explorer's and its engine processes', not one
      run's. An engine in the explorer's process, as hvx's, counts with the explorer.
    - The estimate assumes that each shard runs on a machine like this one and that shrinking
      takes the same share of a longer exploration. It leaves out a shard's build and download.
    - Only the Mac engine ran it on a VM, with an image without `errors:` and without watched
      error paths. hvx and KVM did not run it; the scripted engine covers the error kinds.
    - An exploration made before M336 has no `stats.json`, and `hydo stats` says so instead of
      reading `events.jsonl`.
    - The waits for the Mac's shared VM lock are outside hydo, so the workers' time does not show
      them.
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
- M219 (tentative number): several vCPUs on the KVM engine, stages 2 to 4. `--vcpus N` runs 2 to 8
  vCPUs in turns of counted exits with an x2APIC per vCPU in the engine and the pinned SMP kernel
  `bzImage-kvm-smp`. A determinism bug is fixed: Linux cleared CR4.TSD on every CPU but the boot
  CPU, so a program's RDTSC there read the host's TSC. With the fixed kernel, every schedule of
  Redis on 2 and 4 vCPUs and of 31 nats settings gave one fingerprint on two AMD models. A run from
  S1 gave the fresh boot's. nats-server's flaky test passes at 7 of 28 turn settings on 2 vCPUs; one
  vCPU gave its recorded values; nothing recorded moved
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
- M224 (tentative number): the Blnk onboarding, the third experiment in business bugs, on the
  Mac engine and on hvx. Two new Blnk bugs on `main` have tested patches. Coalesced and
  recovered draws ignore their overdraft limit (found by the overdraft rule alone). An atomic
  bulk keeps its first transfer when a later one fails before it is queued (the bulk rule, the
  generic rule, and the error logs). Acknowledged commits of holds are lost with Redis's defaults
  (the generic rule alone). Nothing recorded moved
- M225 (tentative number): what the Lago onboarding lacked. Eventually checks after the eventually
  commands (`after: commands`), the first commands' records in the run (`test.first_records`), a
  failing check's output in `result.json`, `--shrink-causes`, `services[].job`, and clock steps to
  a time (`T=<at>:@<point>`, `clock_points:`). The Lago example without its workarounds still
  finds lago-api 6901, and a step to the billing tick found a subscription invoiced twice.
  Nothing recorded moved
- M226 (tentative number): a pause of threads of the workload,
  `P=workload/<thread>[#<k>]@<at>~<off>:<dur>`, with the guest kernel's cgroup v2 freezer, and
  `hydo explore --pause-thread`. On hvx a pause of Qdrant's test thread fails its flaky test on
  every run, its neighbours pass, and the fixed test passes. The Mac and KVM engines refuse it
  (no cgroups in their guest kernels). Nothing recorded moved
- M227 (tentative number): `hydo explore --backend kvm --vcpus N` draws turn lengths in exits
  (`--turns`) and searches them (`--sweep turns`); on the nats example both find passes and
  failures, and every failure reproduced. M219's kernel fix holds on Intel. On Intel a run from a
  worker's S1 on 2 vCPUs differed from a fresh boot. The snapshotting machine now goes on in a
  fresh VM; its proof run passed on five Intel and three AMD hosts. Nothing recorded moved
- The 0.3.2 release (no milestone number): the version raised, `hydo-world`, `hydo-world-tls`
  and `hydo-grpc` re-pinned, and the SMP guest kernel in the Linux release. Release step 6a ran
  on quick candidates, and its fixes are in: one install block per method in the README, what
  `hydo init` writes, and a `hydo build --check` warning for its placeholders. The released
  schemas are the current ones, the explorer's full identity golden is re-recorded, and the
  examples are re-recorded (epoch 2). Both releases are built twice to the same bytes, the Mac one
  signed and notarized, and verified on hvx, the Mac and KVM (GitHub's runners). A first build
  was not published: its KVM run showed that on Intel a KVM fingerprint holds for one CPU model,
  and the pages were corrected. Published on 2026-11-14
- The 0.3.3 release (no milestone number): the version raised, `hydo-world`, `hydo-world-tls` and
  `hydo-grpc` re-pinned. Release step 6a ran on quick candidates with two readers who played new
  developers. Its fixes are in: the `hydo: VERDICT` line and terms defined where first used.
  `hydo onboard` comes first in both quickstarts and prints an ordered `First:`/`Next:`. `--check`
  warns of `hydo compose`'s placeholders, and a report says why its command is not shrunk.
  The released project schema is the current one, and the examples are re-recorded (epoch 2).
  Both releases are built twice to the same bytes, the Mac one signed and notarized, and verified
  on hvx, the Mac and KVM (GitHub's runners); published on 2026-11-14
- M228 (tentative number): `hydo report --html` in plain words, from the user's approved mockups.
  It says what broke, when it happens, whether it is a bug, what hydo saw, and how to see it
  again. The specialists' details are folded at the bottom. Every schedule token has words
  (`hydo_schedule::describe`) and every failure class a label and advice
  (`hydo_explore::advice`). Nothing recorded moved
- M229 (tentative number): `hydo onboard` finds a repository's parts, starts `hydo/` with
  `hydo compose` or `hydo init`, installs the skill for the AI tools it finds, and prints the next
  step. The plugin is `hydo` in `plugin/` and the skill `onboard`, so the command is
  `/hydo:onboard` (skill 2.0.0). docs/ONBOARD.md has a worked messages example that runs on the
  Mac engine. Nothing recorded moved
- M230 (tentative number): `hydo report` prints an exploration's problems as the page does, in
  plain words, and `hydo report --details` prints the technical report. `hydo explore` in a
  terminal ends with how many runs failed and in how many ways; outside a terminal, or with
  `--json`, it ends with the summary line as before. The README's terminal shows a real run.
  Nothing recorded moved
- M231 (tentative number): the onboarding of Temporal 1.32.2 with business rules on the Mac
  engine and hvx. The new candidate is a server down for the length of a backward clock step after one
  crash of PostgreSQL, because the SQL handle's reconnect throttle reads the wall clock.
  It reproduced natively. `hydo onboard --compose FILE`, and a messages check's `duplicates`
  with nothing acknowledged no longer ends a run in ERROR. Nothing recorded moved
- M232 (tentative number): the onboarding of Valkey 9.1.3 with `hydo onboard` on the Mac engine, with
  a primary and two replicas. The new candidate is a backward clock step that delays a replica's
  full sync by its size. hydo found it blind, and it reproduced natively. The known issue 5354
  was found blind too.
  `hydo save` inside `hydo/` fixed. Nothing recorded moved
- M235 (tentative number): one VM reaches Jepsen's NATS 2.12.1 findings 3.3 and 3.5 with the
  storage faults of one service on hvx. A node's power loss (`L=`) loses acknowledged messages,
  and a node's corrupted metadata snapshot (`G=`) deletes the stream. 3.2 is not reached; it
  needs two corrupted nodes in one run and a larger stream, not a second VM. Nothing recorded
  moved
- M239 (tentative number): what an onboarding of a project's own binaries lacked: `ready:
  tcp:<port>`, service logs in `<run>/logs/`, and `hydo run --memory`. A memory refusal names
  the inspect sessions. `--check` warns about missing sources. The guide gained `minimum-path`,
  the static musl recipe and the messages value rules. The skill (2.0.1) finds an installed
  hydo off `PATH`. Five frictions of the Valkey onboarding are fixed too. Nothing recorded moved
- M240 (tentative number): what a user's first explorations of a message queue taught. The
  drain outlasts leases and recovery inside `workload.timeout`, with a shell drain in `hydo guide
  messages`. An `eventually_` command retries by itself. A clock step against durations on the
  wall clock is a real finding. A long step reads in hours and minutes. Skill 2.0.2. Nothing
  recorded moved
- M238 (tentative number): upgrade testing. `services[].versions` and `U=` switch a service to
  another version during a run, member by member, with rollbacks. `hydo explore --upgrades` draws
  them. An etcd cluster grown with learners reaches etcd issue 19557 ("too many learner
  members in cluster") on 3.6.0-rc.2 and 3.6.0 with one token. The documented path through
  3.5.26 passes. Nothing recorded moved
- M236 (tentative number): rule suggestion, first version. `hydo rules scan` lists what a
  project promises: its schema, state columns, API, documentation, and tests. `hydo rules check`
  validates candidate rules on runs without faults, with planted changes. The skill (2.1.0)
  proposes rules from the project's own sources and shows the owner only the validated ones. On Blnk, Lago, and the
  never-seen listmonk, 23 of 25 rules held on clean runs. On the examples' saved regressions they
  caught Blnk's overdraft bug, Blnk's lost commits, and all four Lago findings. Blnk's
  atomic-bulk bug was caught only by a rule written after the miss. Nothing recorded moved
- M233 (tentative number): the River onboarding (a job queue for Go on PostgreSQL), started with
  `hydo onboard` and run on hvx. A crash of PostgreSQL gives two River leaders for seconds, and
  both insert one run of a periodic job, because River's leader table is unlogged. Reproduced
  without hydo in 5 of 30 crashes and 0 of 30 clean restarts; no report found. Four fixes of the
  messages check across clock steps and power cuts, and `hydo onboard` names the release's image
  fetcher. Nothing recorded moved
- M237 (tentative number): performance tests from deterministic runs. `perf:` in the manifest
  runs each step as its own call, `result.json` gets `perf` (hvx: retired branches per step,
  from the engine's `HYDO_WORK_RECORD`), and `hydo perf compare` names each change in words. On
  hvx 0.1 % more handler work showed as +0.088 % in every run, and an unchanged build as 0.000 %.
  Nothing recorded moved
- M241 (tentative number): function contracts, a prototype. `// hydo: requires` and `ensures`
  comments, compiled with DWARF into a plan; the Mac engine checks every call at hardware
  breakpoints without changing the run (4 of 4 fingerprints equal). A planted double debit
  breaks `ensures` only under a pause or a lost answer (Rust and Go). uprobes on hvx cost
  6.9 µs per stop and change the fingerprint. Nothing recorded moved
- M243 (tentative number): `hydo k8s` writes a starting hydo.yaml from Kubernetes manifests, a
  Helm chart or a Kustomize folder, through the Compose importer; `hydo onboard` runs it. Secret
  values stay out of hydo.yaml through the format-1 addition `services[].env_file`. Bitnami's
  Redis chart and the voting app's manifests ran on hvx. Nothing recorded moved
- M242 (tentative number): ready-made emulators of outside services. `emulators:` with `use:
  stripe | smtp | s3` runs stripe-mock, Mailpit or rclone's S3 server from a pinned image, behind
  a fault proxy of the same name; `idempotent: true` on an `http` proxy's route. The checkout
  example's double charge found blind on hvx. Skill 2.2.0. Nothing recorded moved
- M234 (tentative number): a pause of threads on the Mac and KVM engines. The fingerprint epoch 3
  gives their images guest kernels with cgroups (`Image-explore-cg`, `Image-explore-netns-cg`,
  `bzImage-kvm-cg`, `bzImage-kvm-smp-cg`); an image built before keeps its kernels and its
  values. Qdrant's flaky test fails under a pause of its thread on the Mac and on KVM with one
  vCPU, and the fix passes. Every recorded value moves and waits for the next pass (open item
  105); on KVM with one vCPU the quickstart's `C=12 R=everysec` passes at epoch 3
- M252 (tentative number): open item 102. KVM fingerprints differed between Intel CPU models
  only in DR6's bits 11 and 16, which KVM reads back from the hardware. From the fingerprint
  epoch 3 on they leave the fingerprint, so Intel models agree; older images keep their values.
  `HYDO_KVM_DIAG` writes every part of the end state. Nothing recorded moved
- M247 (tentative number): the report gaps of 0.3.3's fresh readers. `hydo report <run dir>`
  prints the run page's plain summary, and `--details` the old text. An exploration's problem
  has numbered steps, its own `hydo ci accept` lines, and the failing service's last log lines
  from one more run that `hydo explore` makes (`--no-diagnose` skips it; run on the Mac). A
  service that did not start again is one problem. The disk-error page starts with the disk.
  `hydo explore` prints its fault kinds in words, with the JSON in `fault-space.json`. A replay
  takes the run conversation's schedule, not an explorer boot's. Nothing recorded moved
- M248 (tentative number): the clock audit. `hydo rules scan --kind clock` finds code that
  measures a duration with the wall clock, ranks one place per function, and proposes
  clock-step schedules for the 5 most likely. With each example's hydo.yaml the known places of
  Valkey, Temporal and River rank in the top 5, and 122 of 130 sampled places are real.
  Its Valkey schedule reproduces valkey-io/valkey#5406 on the Mac engine. Skill 2.3.0. Nothing
  recorded moved
- M249 (tentative number): `hydo fake from-openapi` writes a fake from an OpenAPI 3.0 or 3.1
  description. It writes routes, bodies from examples or schemas with templates, `save:`/`load:`
  pairs, the idempotency header, and the error answers in the new route key `errors:`. The
  openapi-payouts example's retry bug showed on the Mac engine, and the fix passed. Skill 2.3.1.
  Nothing recorded moved
- M246 (tentative number): k3s in the guest on hvx, an experiment. The image carries the guest
  kernel's own modules, so no kernel changes. Two boots give one fingerprint; a StatefulSet's
  pods elect a leader on a Lease, and a forward clock step gives two leaders at once. The Mac and
  KVM kernels lack the options; an opt-in variant is designed, not built. Nothing recorded moved
- M245 (tentative number): `G=` on several services, as a list or a group's selector, and
  `hydo explore --corrupt-several`. The strict power loss `L=...:strict` turns off the periodic
  writeback from the run's start and silences the node while it is down. On hvx the NATS
  example's `blocks/` reaches Jepsen's finding 3.2 with them. Corrupted middle blocks on two of
  three nodes leave the replicas different, and lose 2,456 acknowledged messages when the intact
  node is killed. Nothing recorded moved
- M253 (tentative number): web apps through their own Playwright tests. `browser:` runs a team's
  Playwright tests unchanged in a pinned Playwright image and watches every page for uncaught
  errors, spinners that stay, failed requests the page never shows, and broken texts. On hvx the
  React example `web-shop` gave one fingerprint per schedule; its flaky test fails on every run of
  one `H=` delay and passes without it; its stuck spinner was found blind. hydo-build leaves out
  layer entries with a backslash in the name. Nothing recorded moved
- M250 (tentative number): the Hatchet onboarding (a task queue and workflow engine on
  PostgreSQL), started with `hydo onboard` and run on hvx. A crash of an engine loses the messages
  it read from Hatchet's PostgreSQL queue: acknowledged scheduled runs and triggers never run, and
  finished runs run again. Reproduced with Docker, no upstream report found. A cancelled run that
  becomes `COMPLETED` (issue #2655) and a scheduled run that runs twice (issue #4129) are known.
  Nothing recorded moved
- M244 (tentative number): the epoch-3 re-recording. Every value of `recorded.json` is
  re-recorded at epoch 3 on hvx, the Mac and KVM (AMD hosts); on hvx only the image bytes moved.
  Two KVM verdicts changed: the quickstart's lost write is `C=11 R=everysec` on KVM now, and
  Qdrant's KVM failure is a pause of the test's thread. Open item 105 is empty
- M251 (tentative number): the Saleor onboarding on hvx, with rules from the rule suggestion on a
  project hydo had never seen. Two new candidates were found blind and reproduced natively. They
  are reported as saleor#20022 and #20023: a refund request lost with its Celery task, and a gift
  card charged without a record.
  Two known bugs reproduced, and the scan misses Django's `models.py`. Nothing recorded moved
- M257 (tentative number): `hydo explore --protocol-cover` gives each schedule one `H=` token
  from a walk over the cells of the fakes and proxies. A cell is a route, a step, and a rule
  type; the pairs that a probe run saw called come first. On hvx the web-shop double order came
  at seed 3 and its stuck spinner at seed 4. The random draws first lose the order's answer in
  its step at seed 120. openapi-payouts' retry bug came at seed 3 instead of 28. The fixed images
  pass. Nothing recorded moved
- The 0.3.4 release (no milestone number): the version raised, `hydo-world`, `hydo-world-tls` and
  `hydo-grpc` re-pinned, and the four guest kernels with cgroups in both releases. Release step 6a
  ran on quick candidates with two fresh readers, and its fixes are in. The pages name the epoch
  and the engines' vCPUs right. A power cut reads as after N steps, `--check` names
  fetch-images.sh, `hydo onboard` prints one list, the accept lines are explained, and `hydo
  module build --dry-run` builds nothing. The released
  project and results schemas are the current ones, and the examples are re-recorded (epoch 3).
  Both releases are built twice to the same bytes, the Mac one signed and notarized, and verified
  on hvx, KVM and the Mac; published on 2026-11-15
- M254 (tentative number): web apps, continued. A passing Playwright test is acknowledged work,
  so a clean run is not called vacuous. The pages' screenshots go to the run directory's
  `screens/` after the verdict, and `hydo report --html` shows them under the finding that names
  them. `browser.screens: compare` checks each page against a recorded reference, pixel for
  pixel. On KVM (seven GitHub hosts, Intel and AMD) the web-shop example gave hvx's verdicts and
  the same screenshots on every host, and `hydo build` fetched the Playwright image itself. Some
  runs first stopped in early boot, cause not found. Nothing recorded moved
- M256 (tentative number): the guest kernel for containers, `guest.kernel: containers`, an opt-in
  variant named in the image record (`kernel_variant`): `Image-explore-containers`,
  `bzImage-kvm-containers` and `bzImage-kvm-smp-containers`, each built twice. On the Mac engine
  and on KVM a probe runs runc, overlayfs, NAT, conntrack, and ipsets. k3s does not start: kine
  loops without VM exits (open items 13 and 119). Nothing recorded moved
- M255 (tentative number): function contracts, continued. The report counts the calls a run did
  not judge. Contracts read Rust strings, vectors, slices, `Option` and `Result` and Go strings,
  slices and later results. The KVM engine stops at them with three debug registers and finds a
  position-independent program's load address. The hvx side module is designed. On the Mac every
  proof gave one fingerprint with and without the plan, and the one-off KVM workflow passed on
  two Intel and two AMD models. Nothing recorded moved
- M260 (tentative number): function contracts against business rules on Blnk, on the Mac engine.
  Contracts on three functions where money moves found nothing new in 24 blind schedules. The
  overdraft contract broke in the same runs as the rule and named the call and the draw's values;
  in two of three runs the end state hid the overdraft. The cause, a `float64` limit in a map,
  cannot be stated, nor the atomic bulk. Go named slices (`math/big.nat`) are read now. Nothing
  recorded moved
- M258 (tentative number): hydo walks through a web app's pages itself. `browser.explore` replaces
  the tests with a seeded walk named by `I=`. The explorer draws one for every schedule, and the
  shrinker gives it fewer actions or leaves actions out. Each run leaves the walk as a Playwright
  test, and a recording replay adds a video and a trace. On hvx the web-shop example without
  tests had both planted bugs found blind in 20 schedules. Memos 26.11.0 onboarded: no saved memo
  lost in 40 walks with faults. Two new candidates: a share link's copy button over plain HTTP,
  and a reaction lost without a message. Nothing recorded moved
- M259 (tentative number): software updates of embedded devices. RAUC 1.15.2 and SWUpdate
  2026.05.1 update one device with two slots and U-Boot's environment, under power cuts at every
  point of an install and a reboot. RAUC held. SWUpdate never flushes a written image before it
  switches the slot, so a cut before the reboot boots the slot's older contents. What hydo lacked
  for devices. Nothing recorded moved
- M262 (tentative number): a plan's `hydo_version: "0.3"` builds on 0.3 and every newer 0.x, so
  a release raises the middle number without breaking projects. An older hydo refuses and names
  the release it needs; a plan of a newer patch only warns; `>=` and `HYDO_VERSION_CHECK=warn` are
  unchanged. Only the old refusal's golden answers moved. Nothing recorded moved
- M265 (tentative number): why KVM boots on GitHub's runners stopped as `SPIN` in early boot. The
  host never stalled. The KVM engine's spin observer looked at the guest's memory setup for 2 s
  at each kick. In a look the guest moves only from one breakpoint to the next. The SPIN
  limit now counts only the guest's own running, and a look gets at most that much time.
  `HYDO_KVM_GAPS` shows where a long stretch's time went. On six hosts every boot with the fix
  gave its fingerprint with no early stop. A 4 GiB boot fell from 25 s to 5 s, and a loop without
  exits still ended `SPIN`. Nothing recorded moved
- M261 (tentative number): the device examples' two hydo errors. The hvx engine pads the
  host-call port's staged bytes, so a line put during the agent's read keeps its first bytes. A
  `cf=` run is cut at the first stop after its disk lost power, instead of waiting at the
  workload's end on the dead disk. If the agent itself waits on it, the run ends in ERROR. The
  examples drop `workload.timeout: 60`. Nothing in `recorded.json` moved; hvx runs with `cf=` did
- M263 (tentative number): a reboot that the guest asks for (`guest.reboot`, the console line
  `HYDO-REBOOT`, the engines' `REBOOT`) on hvx, the Mac, and KVM. Added disks with caches of
  their own (`guest.disks`) on the Mac and KVM. With a real reboot and the slots and the
  environment on separate disks, the SWUpdate candidate holds; RAUC held. Nothing recorded moved
- M264 (tentative number): the gaps of the walks through the pages. A page invariant's signature
  blanks generated ids, and a walk shrinks by whole steps, halves, and single actions. Memos's
  share-link walk shrank from 72 actions to 31. A clock step moves 9 of 72 actions, 24 before.
  An unmatched `watch.failures_of` warns. Recordings no longer end in `TIMEOUT check`: the hvx
  engine's buffer of the agent's lines was full. On KVM one fingerprint per schedule on four
  hosts. Nothing recorded moved
- M266 (tentative number): open item 128. The KVM fingerprints of the SWUpdate reboot schedules
  differed between builds of the image, not between AMD models: cpio put the build's time into
  each bundle. With one image the EPYC 7763, 9V45, and 9V74 agree, with 0, 1, or 14 reboots.
  `make-swu.sh` sets the bundles' times, and `HYDO_KVM_DIAG` covers the boots a reboot ends.
  Nothing recorded moved
- M267 (tentative number): why M264's walker reached neither Memos finding. The share link is
  four controls deep, and M258's walker reached it in 1 walk of 40. The walk now acts in the menu
  or dialog that opened last. It fills only editable fields and clicks only controls that take a
  trial click. It counts the families it used per step. A dropped clock step moves 7.4 of 72
  actions on average over five walks (19.8 for M264's). web-shop's two bugs were found blind, Memos's
  were not. A finding's recording replay records the step the finding names. Nothing recorded
  moved
- M268 (tentative number): open item 105's two `cf=` schedules after M261. The Saleor gift
  card regression breaks both rules at `cf=271` now; `cf=270` had stopped reproducing when its
  image was rebuilt. d11 of the bug benchmark is no longer found at any flush cut. A saved
  regression changes the bytes of the image it tests (open item 132). Nothing recorded moved
- M271 (tentative number): two small fixes. Every test removes the folder it makes in the
  temporary directory when it ends, also when it fails, and `HYDO_KEEP_TMP=1` keeps them. A
  `cf=` run makes the storage faults' mounts and reads in a child process, so a cut at an
  `L=`'s mount reaches its recovery boot (open item 127). Nothing recorded moved
- M270 (tentative number): open item 131, a recording that shows the failing run. A walk with
  `rec` repeats the actions that the same schedule without `rec` took, by the same role and
  name. Each run prints them as `walk-act` lines. A missing control is left out and named.
  Five Memos recordings repeated their run's 72 actions exactly. The reaction finding and
  web-shop's two bugs kept their verdicts when recorded; the share-link finding has no
  recording, because the new walker's image changes the walks that reached it. Nothing
  recorded moved
- M272 (tentative number): the Mender client 5.2.0 in standalone mode on a device with A/B
  partitions and U-Boot's environment on one eMMC, with real reboots. Two new candidates. An empty
  version file of the Artifact scripts after a power cut leaves an update that can be neither
  committed nor rolled back. An interrupted commit makes the next commit roll back and exit 0.
  mender-artifact writes state scripts in a random order. Nothing recorded moved
- M274 (tentative number): loops without VM exits. Of GitHub's runners only the Xeon 6975P-C
  hosts count a guest's branches, exactly with `0x11c4` and 26 branches late. `guest.branch_tick`
  makes the KVM engine stop a vCPU after 2^26 user-mode conditional branches without an exit, at
  the exact count, and deliver the timer there. k3s's kine loop gives way, and k3s-lease runs on
  KVM with 2 vCPUs, one fingerprint per schedule. The Mac has no such counter and refuses the
  key. Its observer looks at the return address, so TigerBeetle boots on the Mac. Nothing
  recorded moved
- M275 (tentative number): function fuzzing from contracts. `hydo fuzz` generates inputs from
  a Rust function's `requires` and `input` lines, calls it in a copy of its crate, judges each
  call by `ensures`, and shrinks a failure to an input that replays. It found the
  contracts-ledger bug from the contract alone; on Qdrant's `common` crate nine of 12 functions
  held, and a decoder panics on a header its encoder never writes. The VM run is designed only.
  Nothing recorded moved
- M281 (tentative number): the onboarding playbook, `docs/ONBOARDING-PLAYBOOK.md`. It is one
  process for every onboarding of a real project. It covers the choice of a target, the rules that
  worked, the faults that found bugs, five phases with time limits, and the judgment of a finding. It also covers the
  write-up, the upstream report, the retro, throughput, and common setup problems. Skill 2.3.2 links it. Nothing
  recorded moved
- M280 (tentative number): an evaluation of `hydo fuzz` on six crates as a new user. The
  harness fails on `no_std` crates and private fields of other modules, runs that test nothing
  pass, and short values miss real paths. Two candidates in num-integer and byteorder, not
  posted. A developer would not keep using it yet. Nothing recorded moved
- M276 (tentative number): the Mender example on hvx. The x86_64 tools build on the Mac under
  QEMU, with each compile command tried again after a compiler crash; two builds still differ
  in `mender-update`. Finding 1's regressions give their Mac verdicts; finding 2's give its
  violation in the earlier form, and a cut 69 to 87 ms after the event gives the Mac's form.
  Each schedule gave one fingerprint over two runs. Nothing recorded moved
- M273 (tentative number): a second web app for the walk, Vikunja 2.8.0 on SQLite. The walk
  records the ids of a REST API's answers (`<METHOD>:<path>=<id>`), and `watch.failures_of`
  matches the method. There are two new candidates, not posted. A power cut loses the tasks that
  Vikunja acknowledged in the last seconds (WAL with `synchronous=NORMAL`). The quick actions
  dialog loses a task that the server refuses and leaves the error unhandled. Nothing recorded
  moved
- M278 (tentative number): the onboarding of Dragonfly 2.1.0, one primary and two replicas, on
  hvx. In a new candidate, Dragonfly loads the snapshot with the latest file time. After a
  backward clock step, a restart (also a clean SHUTDOWN) loads a snapshot from before the step
  and loses every saved write since. It was found blind and shrunk to `T=8:-18`. A power cut loses acknowledged
  snapshots (known, #8646), and Dragonfly's Lua needs AVX (known). REPLTAKEOVER kept its promise.
  Nothing recorded moved
- M277 (tentative number): the SurrealDB 4.0.1 onboarding on SurrealKV, on hvx, and on the Mac
  engine. A new SurrealKV candidate: after a damaged value of the value log, the next open drops
  every later acknowledged commit when none of them has a large value. With one, it refuses.
  Found blind 7 times in 167 schedules, reproduced with Docker, not posted. Two known SurrealDB
  clock bugs reached; the experimental upgrade from 3.3.5 held. Nothing recorded moved
- M279 (tentative number): the Redpanda v26.2.7 onboarding, three nodes on hvx, with a messages
  check over idempotent produces, transactions, and a consumer group. Strict power losses and
  whole-guest cuts lost nothing. Three new candidates. A damaged `tx.snapshot` stops its node at
  every start. A transaction recorded as `empty` by its coordinator is never aborted. A
  committed transaction is visible on only one of its partitions. A follow-up from KVM: after a
  change of leader, a produce answer names an earlier transaction's offset, and a new request
  can get one without being written. The messages check's `duplicates` message names a wrong
  acknowledged id. Nothing recorded moved
- M282 (tentative number): what the evaluation of `hydo fuzz` found, fixed before 0.4.0. The
  harness builds on `no_std` crates and around private fields, function by function. Each
  function ends with a verdict, and `vacuous` and `too few inputs` exit 3 with a fix. `input`
  takes lengths, characters and examples, sizes stay bounded, and `with T = i64` fuzzes a
  generic function. On the six crates every harness built, and the planted memchr bug and
  num-integer#55 came from the functions' own contracts. Nothing recorded moved
- M269 (tentative number): open item 132. The saved regressions leave the image: `hydo build`
  writes them beside it as `<image>.regressions.json`. An edited, new, or removed regression
  leaves the image's sha256 the same (hvx and the Mac), and an `# expect: fp=` line holds its
  own image's value. The fingerprint epoch is 4; an image built before keeps its regressions
  inside. Every image built at epoch 4 has new bytes, and its values wait for the pass before
  0.4.0 (open item 105)
- M283 (tentative number): the guest mounts cgroup2 on `/sys/fs/cgroup` at every boot from the
  fingerprint epoch 4. The host controller sends the agent's `mount` before S0, so the agent pin
  and the images' bytes stay; `guest.cgroup2: false` turns it off, and an older image mounts
  nothing. Dragonfly and Redpanda start on hvx without their `setup:` mounts. The pinned
  reproducers keep their values. Every epoch-4 run moves and waits for the pass (open item 105)
  committed transaction is visible on only one of its partitions. Nothing recorded moved
- M285 (tentative number): value history. `// hydo: key` and `// hydo: record` lines record
  values at every call through the contracts' breakpoints, and `hydo inspect values` shows them
  per key with each change. Floats, strings, Go interfaces and small Go maps are read. On Blnk
  the overdraft limit is 50.0 in the API, absent from the saved metadata, and 0.0 in the
  worker's coalesced copy. One fingerprint with and without the plan. Nothing recorded moved
- M286 (tentative number): hydo in a team's CI, what makes a team keep it. `hydo ci page`
  writes the history store as one page that loads nothing: each finding across commits and
  branches, a timeline, and the runs. The pull-request comment shows only what the change did,
  each finding with its replay and accept lines, edited in place. A `workflow_run` recorder
  comments on and records pull requests from forks. `seeds_from: history` turns a budget into a
  seed count. Nothing recorded moved
- M284 (tentative number): web apps on the Mac engine. `guest.root: disk` (a format-1 key, the
  default with `browser:`) puts an arm64 image's root on its disk as a read-only ext4 file
  system. `hydo build` makes it in a helper VM, and a small initramfs mounts it. The guest kernel
  did not change, and no run ended in `SPIN`. web-shop's flaky test fails on every run of its
  schedule, the walk found both planted bugs blind in 20 seeds, and a Memos walk and its
  recording gave hvx's verdicts. Each schedule gave one fingerprint over two runs. Nothing
  recorded moved
- M288 (tentative number): a second evaluation of `hydo fuzz` as a new user, on strsim, hex,
  httparse, bytes, num-traits and chrono. Candidates: strsim's `sorensen_dice` divides by byte
  lengths, httparse accepts a chunk size line without a digit, and num-traits' `abs_sub` returns a
  negative difference. A clause on an `f64` result passes unchecked. Nothing recorded moved
- M289 (tentative number): the blockers of M288 fixed. `hydo fuzz` reads and generates `f32`
  and `f64` values, and a clause it cannot decide is `not judged`, never `held`. The harness
  builds under `#![forbid(unsafe_code)]` and keeps the `[lints]` tables. Skipped functions exit 3
  (`--allow-skipped`). `like b"..."` and `--show-inputs N` are new. Nothing recorded moved
- M291 (tentative number): value properties. `// hydo: keep <name> [once set] [from <function>]
  [to <function>] [across boots]` judges a recorded value along each key's history, and a broken
  line is a `values` finding and a `VIOLATION` that names the line, not the key. On Blnk the run
  without faults, where every business rule held, now fails, and the fixed build holds. In 30
  blind schedules the line broke in 9 (first seed 4), the overdraft rule in 1 (seed 21). One
  fingerprint with and without the plan. Nothing recorded moved
- M290 (tentative number): a cheaper fingerprint, part of the fingerprint epoch 4. On the Mac
  and KVM engines the disk's part is a tree of 4 KiB block hashes. The base's hashes are made
  once per process while the guest runs, and only written blocks are hashed again. web-shop's
  run takes 8 s in the engine instead of 42 to 55 s. Older images keep their values. Every
  epoch-4 Mac and KVM fingerprint moves (open item 105)
- M293 (tentative number): the guide topic `hydo guide strategies`. It lists the 28 techniques
  that found bugs with hydo. Each entry says what to write, when it fits, what it found and
  missed, and its cost. A first table maps a type of system to the techniques that found most.
  The playbook and the skill (2.3.3) point to it, and the retro updates it. Nothing recorded
  moved
- M292 (tentative number): `hydo fuzz --vm` fuzzes Rust functions inside hydo's VM under power
  cuts, disk errors, clock steps and pauses, and `// hydo: durable <check>` checks each call
  again after a power cut. On hvx redb's commits held, and a save by rename without a sync lost
  its text at every cut. The verdict: keep it for code that writes files. Nothing recorded moved
- M300 (tentative number): `docs/USE-CASES.md`, every use case that hydo serves or could serve.
  It maps the techniques of a common chart of testing stages to hydo's commands, each `built`,
  `partly built`, `idea`, or not hydo's job, with its evidence. It lists the questions of seven
  kinds of teams and 33 ideas. Each idea names its need and its source. "How to record new
  work" gains step 4. Nothing recorded moved
- M297 (tentative number): a new user's trial of 0.4.0's features on the Mac engine, from the
  guide alone (`docs/design/0.4.0-user-trial.md`). Web apps, value properties, CI, `hydo fuzz`,
  and the strategies topic each gave a first result within minutes. The fixes before 0.4.0: one
  step numbering, a true CI history page, and walk recordings that show the failure. Nothing
  recorded moved
- M294 (tentative number): a cheaper RAM part of the fingerprint, part of the fingerprint epoch
  4. On the Mac and KVM engines guest RAM is hashed by a tree of 4 KiB page hashes on half of the
  cores. KVM takes the hashes of the pages not written since its base from the base. web-shop's
  RAM hash takes 43 ms instead of 1 s, and its run 5.8 s in the engine instead of 7.9 s. Older
  images keep their values. Every epoch-4 Mac and KVM fingerprint moves (open item 105)
- M298 (tentative number): differential testing between versions. `hydo compare-versions` runs
  given, saved or drawn schedules on two versions and compares the verdicts, the client's
  outcomes and histories, and the stored state (`services[].compare`), with normalization rules.
  It shrinks drawn differences and writes `compare.json`. On the Mac engine Redis `always`
  against `everysec` differs on 4 of 10 drawn schedules. Two builds of one source and `hz` agree.
  Formance v2.4.0 shows the account race that v2.3.24 does not. Nothing recorded moved
- M296 (tentative number): faults inside a call and the Mac engine for `hydo fuzz --vm`. A named
  call (`E=`) reports its start, and a reaction cuts the power, steps the clock, pauses the call
  or its thread, or kills it a few milliseconds later. Planted wall-clock and missing-wait bugs
  broke and their twins held, rs-snowflake gave a smaller id after a backward step, and the Mac
  repeated M292's results. A host timeout per run, and native calls in folders of their own.
  Nothing recorded moved
- M299 (tentative number): configuration as a dimension that hydo explores. `configs:` lists a
  build parameter's values with `tradeoff:` notes, and `hydo explore --configs` explores one
  image per combination with the same seeds. Its table gives each combination's failures with
  minimal schedules, the values each failure needs, and a "documented trade-off" label (results
  type `configs`). On the Mac, Redis's `everysec` and `no` and NATS's default and 10 s
  `sync_interval` lost acknowledged writes at every power cut of seeds 1-20, by design, and the
  durable values passed. Nothing recorded moved
- M301 (tentative number): the trial's frictions fixed. Its stuck-spinner problem was the
  watcher's: a spinner of a page that the walk left stayed open; a new document ends it now. The
  report links every recording and says when one did not fail. Steps carry the schedule's
  numbers everywhere. The CI page's headline counts every line, and runs show their start
  (`started`). `hydo setup` names the guest files a release lacks, and `HYDO_RELEASE_DIR` runs a
  tree build. Nothing recorded moved
- M295 (tentative number): the Meilisearch v1.57.0 onboarding on the Mac engine, against what
  its task queue promises. Three new candidates, not posted. An index swap runs twice after a
  crash between its two commits, also after a SIGKILL in Docker. A power cut empties an enqueued
  addition's payload, and a succeeded dump is empty after a power cut. Nothing recorded moved
- M287 (tentative number): the TiKV v8.5.8 onboarding on hvx: three PD members and three
  stores. Snapshot isolation, linearizable raw registers, the bank, and acknowledged writes held
  under every fault kind. One new clock-effect candidate: after a backward clock step, PD's
  timestamps keep their physical part, and TiKV judges lock expiry by it. So a crashed
  transaction's locks block writers for the length of the step. A known PD bug was reached
  blind: tikv/pd#11596, a member that restarted shortly before a power cut never starts again.
  Nothing recorded moved
- M303 (tentative number): the onboarding of Restate v1.8.1-hotfix.2, with a deployment on its
  Rust SDK. An effects file beside the journal, four rules, and a messages check. One new
  candidate, not posted: after a backward clock step, a restart of the server holds accepted
  invocations until the wall clock reaches their creation time. The explorer reads an image's
  `guest:` with its parameter values. Nothing recorded moved
- M302 (tentative number): the epoch-4 re-recording before 0.4.0. Every value of `recorded.json`
  is re-recorded at epoch 4 on hvx, the Mac and KVM (AMD hosts). Every image and every run
  moved on all three. Two verdict changes are accepted. Five findings have new schedules. They
  are Qdrant's pause at 208 ms, nats at `q=19783`, Redis's liveness pause at step 17, the racy
  composer at `q4`, and etcd 14685 by its first symptom. A tick jitter that delays no
  timer at epoch 4 is replaced. Whole-RAM dumps showed that the RAM's tree hash follows the RAM.
  Open item 105 is empty
- M304 (tentative number): explorations spread over GitHub's KVM runners. `hydo explore --shard
  I/N` explores one part of a seed range, and `hydo explore --merge` makes one exploration of the
  parts without a VM, equal to one exploration on the scripted engine. The workflow `explore.yml`
  builds once, explores up to 20 parts and merges them; `ci/remote-explore.sh` starts it. Redis
  in 8 parts gave one runner's verdicts; Dragonfly in 20 parts found its finding in 31 minutes
  against 75; Redpanda found findings 1 and 2. Nothing recorded moved
- M307 (tentative number): `hydo fuzz` covers enumerations, trait methods, and types built by a
  constructor. `matches(e, <pattern>)` names a variant, `with Self = <type>` fuzzes a trait's
  method on a type, and `build` and `read` lines make and read a type with pointers, such as
  bytes' `Bytes`. `requires sorted(v)` sorts the generated list. On the second evaluation's six
  crates and byteorder, refused contracted functions fell from 59 of 66 to 2; no new finding.
  Nothing recorded moved
- M306 (tentative number): which error paths ran. `error_paths: true` lists a program's
  error-handling places from its sources and debug information. The Mac engine watches each with
  a one-shot `BRK` in guest RAM without a change of the run. `hydo report --paths` lists what
  only faults reached and what never ran. On Blnk 46 of 1,326 places ran only with faults, 27 also
  without, and 1,253 never; every fingerprint stayed. `hydo explore --no-faults`. Nothing
  recorded moved
- M305 (tentative number): a kill at the moment of a console event. `kill-now-<svc>` kills a
  service from a real-time relay in the guest that the event's own write wakes, before any
  thread of the service runs on; no engine, agent, kernel or image changed. `hydo explore
  --kill-now` draws it. Meilisearch's swap runs twice at 13 and 14 ms after its `202` on the Mac
  engine and at 34 to 40 ms on hvx, where `kill-<svc>` never did. Old schedules keep their
  fingerprints on the Mac and hvx. Nothing recorded moved
- M310 (tentative number): fixes of explorations on many runners. `hydo explore --merge` adds
  up the parts' error paths. The report's command for more runs names the image without its
  folder. `ci/remote-explore.sh` writes the report again where it downloads it, and `explore.yml`
  passes the example's `known-errors.yaml` to the parts. Nothing recorded moved
- M308 (tentative number): a third evaluation of `hydo fuzz` by a new user, on heck, half,
  either, humantime, percent-encoding and arrayvec. hydo fuzzed 57 of 73 contracted library
  functions and reproduced 9 known upstream bugs, none new. A list of built values gives a false
  `broken`; `Self::Owned` results, a name used by two enumerations, const generics, `&'static`
  parameters and closures stop contracts on the API. Nothing recorded moved
- M309 (tentative number): `--kill-now` on a project with one service. Half of its fault
  reactions, drawn from a stream of their own, kill any service at the event's moment, and the
  schedule's `K=` or `P=` on that service is left out. Explorations without `--kill-now` draw the
  same schedules. On Meilisearch 101 of 360 schedules carried such a kill, against none of 24
  before. Finding 1, the swap that runs twice, was found blind at seed 101 and shrunk to one
  reaction. Nothing recorded moved
- M312 (tentative number): the onboarding of the VictoriaMetrics cluster v1.156.0 on hvx, two
  vmstorage nodes with `-replicationFactor=2`. Without clock steps one node's loss lost nothing,
  and queries returned each sample once. Three new clock-effect candidates, not posted. A step
  back deletes the current month's partition on every node (since v1.141.0). New samples wait in
  memory for the step's length. Spurious `i/o timeout`s leave a batch with one copy. Nothing
  recorded moved
- M311 (tentative number): the ClickHouse v26.10.6.18 onboarding on hvx with two replicas and
  three Keepers. No retried insert was stored twice, with hashes, tokens and async inserts, and
  every acknowledged quorum insert was on the other replica at once. A known bug (#53143): with
  the fsync after a merge and a fetch off, the defaults, a power cut loses merged parts on both
  replicas. Replication then stops for good with rows on one replica. `hydo explore --configs`
  labeled the default settings' loss the documented trade-off. Nothing recorded moved
- M315 (tentative number): `hydo update` updates hydo the way it was installed: Homebrew,
  cargo, the release archive's `install.sh`, or the install cache. It updates every copy of the
  skill too: the known folders, every Claude Code configuration folder, the registry of copies,
  and the marketplace install through `claude plugin update`. Only this platform's archive is fetched,
  checked by sha256. `--check` exits 0 or 3, and `hydo doctor` names a newer release once a
  day. Skill 2.4.0 runs `hydo update --check` first. Nothing recorded moved
- M313 (tentative number): faults of the disk's blocks on the Mac and KVM engines. A torn write
  at a power cut (`torn=`), a misdirected write (`mis=`), bad sectors (`lse=`, and
  `G=<svc>@<at>:bad|rot` under a file), and a request without an answer (`hang=`), with
  `hydo explore --block-faults`. Each broke the careless store of the `disk-faults` example and
  not the careful one, one fingerprint per schedule. hvx refuses them. Nothing recorded moved
- M314 (tentative number): the onboarding of Apache Iggy server 0.10.0 on hvx: one server, and
  a client on the Go SDK. With `durability: persisted`, no acknowledged message was lost, stored
  twice or read out of order, and no stored group offset was lost, in 112 blind schedules. With
  the default `replicated`, a kill alone loses acknowledged messages, which `hydo explore
  --configs` labels the documented trade-off. A backward clock step stops a partition's message
  expiry for the step's length, a clock effect by design. Nothing new. Nothing recorded moved
- M316 (tentative number): five fixes from a user's onboarding with 0.4.0 on the Mac, each its
  own commit for 0.4.1. The reader of `hydo.yaml` takes YAML anchors and `x-` keys, an image's
  FIFOs reach its root, and emulators run with `network: bridge`. The diagnosis claims no lost data when a
  service's environment stopped it. The text about the Mac's service networks and memory is
  true. The missing tmpfs was not reproduced; its race fix waits for 0.5.0. Nothing recorded
  moved
- M318 (tentative number): the first six fixes of the third `hydo fuzz` evaluation, each its own
  commit. Built elements in a list compare as their read values, and `Self::Owned` results are
  read. A name of two modules resolves by the function's module. Const generics, `&'static`
  parameters and closures are fuzzed, default strings hold case-changing characters, and `read`
  lines take `to_string()`, `collect()` and turbofishes. On the six crates, 30 refused or wrong
  of 71 became 0. Nothing recorded moved
- M320 (tentative number): images in private registries. `hydo compose` and `hydo-fetch-image`
  log in with the credential that `docker login` saved: an `auths` entry, or the helper of
  `credHelpers` or `credsStore`, in the registry's token flow or as basic authentication, and
  never print a secret. A 401 says where hydo looked. `hydo compose --from-docker` saves an image
  that hydo cannot fetch from the local Docker and pins its digest. Proven with a registry in the
  test process and with `registry:2`. Nothing recorded moved
- M317 (tentative number): the fingerprint epoch 5. An image service's launcher and its `hydo-oci`
  helpers mount the shared tree, the system folders and the service's `mounts:` in one block per
  boot under one lock. So a readiness check no longer binds `/dev` again over the `/dev/shm`
  tmpfs. On hvx seven PostgreSQL image services showed the race before
  the fix and not after it. Images built at epoch 4 keep their launchers and values. Every
  recorded value waits for the pass before 0.5.0 (open item 105)
- M319 (tentative number): the ClickHouse runs that ended `TIMEOUT workload` after 900 s were
  the guest's clock on hvx, not its agent. `refined-jiffies` counts a tick as 999,848 ns, and the
  periodic tick sets ticks 1 ms apart. So a tick is due 19 ms ahead after 2 minutes, and a busy
  guest's clock nearly stopped. `guest.exact_tick: true` boots it with `clocksource=jiffies`;
  seed 20 then ends with finding 1's verdict. Nothing recorded moved
- M321 (tentative number): two tests that only Linux ran, and command output without a pipe. On
  Linux the launcher test of M317 replaced its own folder's path with the scripts' `/tmp/hydo`.
  The registry test of M320 left image files that let a wrong password pass. Both
  tests are fixed; hydo's code was right. `host_cpu` and `available_mib` read `sysctl` and
  `vm_stat` through a file, so a pipe that another child inherits no longer keeps them waiting.
  Nothing recorded moved
- M322 (tentative number): `hydo update`, `hydo skill update` and `hydo doctor` under GitHub's
  rate limit. They send `GH_TOKEN`, `GITHUB_TOKEN` or `gh auth token` to `https://api.github.com`
  only, and never show it. At the limit the commands say so in one line, with the reset time
  and how to lift it, and exit 4; the doctor stays quiet. One request per command, and a kept
  answer of 10 minutes. Nothing recorded moved
- The 0.4.1 release (no milestone number): 0.4.0 with Milestones 315 and 316, cut from 0.4.0's
  release commit on the branch `release-0.4.1`. The version raised, `hydo-world`,
  `hydo-world-tls` and `hydo-grpc` re-pinned, and the released project schema is the current
  one. The examples are re-recorded at epoch 4: 9 images on hvx and 9 on the Mac moved, as at
  0.4.0. Release step 6a checked the changed pages. Both releases are built twice to the same
  bytes, and the Mac one is signed and notarized. They are verified on hvx, the Mac and KVM (six of
  GitHub's runners). Published on 2026-11-19 with the skill release `skill-v2.4.0`; the branch
  is merged into `main`
- M325 (tentative number): the busy Keeper threads of the ClickHouse runs are a ClickHouse
  candidate. A server that gives up its session sends `Close` and closes its socket. A Keeper
  without a quorum cannot answer, and its handler polls the closed socket without a pause until
  the session timeout of 30 s. On hvx with `guest.exact_tick` two such threads of `keeper2` took
  a third of the guest's CPU. No report found; nothing posted. Nothing recorded moved
- M324 (tentative number): three gaps of a user's onboarding with 0.4.0, each its own commit.
  `hydo compose` writes `root.share_images: true` commented out for services of one image, and `hydo build` warns
  when the roots exceed the Mac's guest RAM or hvx's disk. Each problem of an exploration
  names the `hydo run --out` of its run directory, and `hydo inspect diagnose` of the
  exploration reads it. The clock audit leaves out test code and ranks the code of the Cargo and
  Go programs that the services run first. Nothing recorded moved
- M323 (tentative number): hvx's guest clock right by default from the fingerprint epoch 5. An
  x86_64 image built at epoch 5 boots with `tsc=reliable` and a virtual TSC step of 1 µs, so its
  clocks follow the TSC exactly and read in steps of 2 µs. `clocksource=jiffies` ran a busy
  guest's clock at 0.44, and `tsc=reliable` alone made a read cost 82 µs and broke ClickHouse's
  start. Epoch-4 images keep their values; every epoch-5 hvx value waits for the pass (open item
  105)
- M326 (tentative number): what M318 left of the third `hydo fuzz` evaluation, each its own
  commit. A test module that does not build names its cause in the user's files, and a private
  field's refusal names the `build` line with the crate's constructor. Contracts take bit
  operators, two lists of one length and `{ .. }` patterns; `--fn codec::`, `&'static mut`,
  glob imports and more closure shapes work. The six crates kept every verdict. Nothing
  recorded moved
- M327 (tentative number): a fourth evaluation of `hydo fuzz` by a new user, on semver, base64,
  smallvec, time, textwrap and ordered-float. hydo fuzzed 71 of 88 contracted library functions
  and all 60 property functions, and reproduced 3 known upstream issues, none new. A list of
  unread `A::Item` values gives a false `broken`; associated types of a type parameter, private
  error fields, and fallible or argument-free constructors stop contracts on the API. Nothing
  recorded moved
- M328 (tentative number): small fixes from a user's second onboarding round, each its own
  commit. `hydo explore` exits 5 when it refuses before any run, with a JSON line. `hydo
  inspect stop --all`, sessions named with their memory and age, a 300 s idle time, `.id` at
  the top, and a hint after the second one-shot call. Failure classes name the fault families
  without a power cut. `hydo save` says what unconfirmed means. A root too big for guest RAM
  and a used-up exit budget get their own verdicts. Nothing recorded moved
- M329 (tentative number): three gaps of a user's onboarding with 0.4.1. The s3 emulator's
  proxy keeps the service's `Content-Length` and whole bodies, so uploads through it no longer
  vanish (open item 162). rclone answers conditional writes 412 as S3 does, and the routes
  `put_if_absent` and `put_if_match` let an `H=` rule answer them 412. Both releases ship the
  shell assertion SDK, and `hydo guide hydo_assert.sh` prints it. Nothing recorded moved
- M330 (tentative number): what a user's second onboarding round of a distributed query engine
  taught. `hydo guide strategies` gains four entries: production's topology, the features behind
  a flag, the checks of how each unknown outcome ends, and pauses of a service past its failure
  detector. The skill and the playbook ask for production's shape and for promises from the
  manifests when no owner answers. They read the build's RAM warnings before the first run and
  ask for a second round before a report of no bugs. The skill's version is unchanged. Nothing recorded moved
- M331 (tentative number): each image service has a `/tmp` of its own from the fingerprint epoch
  5. Open item 147 blamed `root.share_images` for TiKV's lock files that met in `/tmp`. But every
  image service bound the guest's own `/tmp`, shared or not, as three services of one image
  showed on the Mac at epochs 4 and 5. The service's `/tmp` is now `/tmp/oci/<service>`, with
  `/tmp/hydo` bound inside. `hydo compose` writes `share_images: true` again: three Dragonfly
  services left 241 MiB more guest RAM with it. The 7 images with image services wait for the
  pass (open item 105)
- M332 (tentative number): the onboarding skill has hydo's version. Its four version fields equal
  `hydo/VERSION`, and `skill_check::` fails when they differ. From 0.5.0 on the release
  `v<version>` carries `hydo-skill-<version>.tar.gz`, and `hydo skill update` and `hydo update`
  take the skill from the same release list. A skill of the old line 1.0.0 to 2.4.0 counts as
  older than any hydo version. The `skill-v` releases go after 0.5.0. Nothing recorded moved
- M333 (tentative number): the fixes of the fourth `hydo fuzz` evaluation, each its own commit.
  No comparison with an unread value breaks; `A::Item` resolves through the `with` line's type;
  an error with a private module's field builds. `build` lines take no arguments and fallible
  constructors, and the refusals name lines that build. The `with` types reach parameters'
  types. On the evaluator's first contracts hydo fuzzed 76 of 88 against 51, and all three known
  issues showed again. Nothing recorded moved
- M334 (tentative number): the failing run's own frames, and the pages read back. Every run of a
  walk keeps a small screenshot of every action with its sentence. A failing run keeps the
  Playwright trace of every step, and a power cut takes them along. `hydo report --html` shows them
  as the run's timeline. `browser.read_back` checks that the pages show every object the server
  acknowledged, also after a power cut. On hvx it found Vikunja's lost tasks from the home page
  alone. Nothing recorded moved
- M336 (tentative number): statistics of an exploration. `stats.json` holds the curve of new
  behaviour with a judgement, the cost by role and by worker, the faults drawn against
  `faults:`, and how many runs fit in a budget; `hydo explore --merge` merges it. `hydo stats`
  prints it, and `hydo report` and its page show it with inline SVG charts. The Redis
  quickstart's seeds 1-60 on the Mac engine were flat over the last 15 runs, and its 2-part merge
  gave the same curve. Nothing recorded moved
- M337 (tentative number): research on shared beginnings of runs. On 13 hvx explorations the
  first runs shared 0.07 % of their time, because each schedule's seed, composition, walk, and
  early first fault reach the guest at once. Other draws would save 3 % to 11 % and lose the
  early faults of 69 % of the failing runs. Shrinks share 10 % to 43 %. A no-go for a tree of
  snapshots; the first version to build takes the result of a repeated schedule. Nothing
  recorded moved
- M338 (tentative number): four fixes at the fingerprint epoch 5, each its own commit. An arm64
  root keeps an image's hard links (the busybox image: 259 MB to 6.4 MB of initramfs). The
  root's estimate counts the whole root and the compressed archive, so PostgreSQL in 1,024 MiB
  warns. An image service binds `/sys` recursively and sees cgroup2's files. A timed pause of
  threads leaves out a thread that ended and takes the largest session. The pinned reproducers
  kept their values; every epoch-5 value waits for the pass (open item 105)
- M339 (tentative number): three explorer and command-line changes for 0.5.0, each its own
  commit. A first run or shrink candidate whose schedule ran before takes the earlier result
  without a VM, and drawn schedules write `dseed=0` where the seed decides nothing. The explorer
  shrinks a service that does not start again and a workload that does not end, but not the
  system's detection of damage unless asked. `hydo run`'s memory refusal exits 5. Nothing
  recorded moved

## Where to look

- `docs/HISTORY.md`: every milestone with its evidence, and the superseded values.
- `docs/USE-CASES.md`: every use case with its status, by testing stage and by who asks, and the
  ideas not built yet.
- `docs/REBASELINE.md`: the recorded fingerprints by section; section 13 is epoch 1, section 19
  is epoch 2, section 24 is epoch 3, section 26 is epoch 4, section 29 is epoch 5.
- `docs/BUILD-PINS.md`: the pinned toolchain and guest programs.
- `docs/CI.md`: what CI runs, where, and how long it takes.
- `docs/RELEASING.md` and `docs/PUBLISHING.md`: the release and publishing steps.
- `docs/design/`: the design of guest control, the KVM engine, the guest's clock on hvx, the
  install cache, signing, `hydo explain`, `hydo inspect`, workloads, and storage faults.
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
- To onboard a project, follow the skill `plugin/skills/onboard/SKILL.md`. After a change, run
  `cargo test -p hydo-agents --test it skill_check:: -- --nocapture`. Put a fact about hydo into
  `hydo/guide/`, never into the skill. Since M332 the skill has hydo's version, and a skill
  change ships with the next hydo release. That release's version bump raises the skill's four
  version fields together (`docs/RELEASING.md`, "The onboarding skill").
- To write documentation, follow the skill `skills/hydo-docs/SKILL.md` and run `sh ci/vale.sh`.
- For kernel and VMX work, use the Intel SDM Vol. 3C (the VMX chapters), the KVM source, and the
  tutorial series Hypervisor From Scratch.
