# Continuous integration

Two workflows (Milestone 167; split into two tiers in Milestone 171). Neither needs a secret;
both read the repository only (`permissions: contents: read`, the checkout keeps no
credentials), pin every action by commit, and run every step with `bash -eo pipefail`.

- `.github/workflows/ci.yml`, the **per-push tier**, on every push and pull request: formatting,
  clippy, and the `fast` tests (below) of the packages the change can affect, with
  cargo-nextest in the `ci` profile; the guest programs; the SDKs.
- `.github/workflows/ci-full.yml`, the **full tier**: every test of the workspace with
  `cargo test --locked --release --workspace`, as docs/RELEASING.md step 1 asks before a release,
  every night (02:17 UTC, on the default branch), on demand (Actions, `ci-full`, Run workflow, or
  `gh workflow run ci-full.yml --ref <branch>`), and on a pull request labelled `full-ci`.

## What runs

| Job | Runner | What it runs | Time |
|---|---|---|---|
| `rust (ubuntu-24.04)` | GitHub's Linux x86_64 runner | `cargo fmt --all --check`, `sh ci/consolidate-tests.sh --check` (below, "One test program per crate"), `cargo clippy --locked --workspace --all-targets -- -D warnings`, then `ci/affected-packages.sh` and `cargo nextest run --cargo-profile ci` with the `fast` tier's filter (`ci/tier-filter.sh`) | about 3 minutes (169 s) with warm caches (6.5 minutes with none) |
| `rust (macos-15)` | GitHub's macOS Apple Silicon runner | the same | about 4.5 minutes (257 s) with warm caches (8 minutes with none) |
| `guest programs (ubuntu-24.04)` | Linux | the guest agent (`hydo/crates/hydo-agent`: its native tests, then the agent as PID 1 of its own PID and mount namespace, which needs `sudo -n` and runs because the runner has password-less sudo) the store reader (`hydo/crates/hydo-read`) and the SQL workloads (`hydo/crates/hydo-sql`, Milestone 182); each keeps its own Cargo workspace and lock file | under 1 minute |
| `sdks` | Linux | the Rust SDK (`sdk/rust`, the reference that writes `sdk/cases/`), the Rust test SDK (`sdk/rust-test`), the Go SDK (the public module `sdk/go`, and the frozen copy `hydo/stack/hydoassert` and `hydo/stack/hydohist` the order service builds with) with Go 1.26.5, the Python SDK with Python 3.9 and 3.13, the JavaScript SDK with Node 22 | about 1 minute |
| `rust full (ubuntu-24.04)`, `rust full (macos-15)` (ci-full.yml) | Linux, macOS | `cargo test --locked --release --workspace --no-fail-fast` (every test, the documentation examples too), then a step that prints what the tests skipped | 18 minutes (Linux) and 29 minutes (macOS) with a warm cache, 23 and 31 with none |

The checks docs/RELEASING.md step 1 asks for before a release are the per-push tier's `fmt` and
`clippy` and the full tier's `cargo test`. The skill check (`hydo-agents`' `skill_check`) also
runs the Agent Skills reference validator: the `rust` jobs install `skills-ref` 0.1.1 from PyPI
into a virtual environment and name its program (`agentskills`) in `HYDO_SKILLS_REF`.

## Where the time went, and what changed (Milestone 171)

Measured on 2026-11-07. The CI numbers are GitHub's step times; the local ones are this Mac (an
M2 Pro, 12 cores), shared with other agents' builds at the time (load averages of 10 to 100, so
the local numbers vary by tens of percent between runs).

**Before (Milestone 167's workflow, run 42379987465, warm cache).** The `rust` jobs took 22.8
minutes (Linux) and 24.0 minutes (macOS). Their `cargo test --release` step took 21.8 and 22.8
minutes, of which compiling took 17 min 16 s and 17 min 51 s and running the tests about 4.5 and
5 minutes (the documentation examples 2 and 3 seconds). `Swatinem/rust-cache` restores the
compiled dependencies, but a fresh checkout's files are newer than any cached build, so Cargo
compiled the workspace's own crates every time, with the release profile.

**Where the compile time goes.** Locally, building every test program from scratch with the
release profile (`cargo test --release --workspace --no-run`) took 534 s; the workspace's own
crates took 6,039 CPU-seconds and all third-party dependencies 305. The release profile's thin
link-time optimisation with one codegen unit makes every one of the 110 test programs and
20 programs a whole-program optimisation of its dependency tree: about 100 s each under load
(`hydo` 208 s, `hydo-control`'s test programs 90 to 130 s each, `hydo-grpc-gen`'s `client`
124 s). The crates with the most compile time, all targets summed: `hydo-control` 1,216
CPU-seconds, `hydo-explore` 586, `hydo-build` 471, `hydo-grpc-gen` 450, `hydo-compose` 446,
`hydo` 399, `hydo-manifest` 284.

The same build with the `ci` profile (the release profile without link-time optimisation, 16
codegen units, `Cargo.toml`) took 132 s with an empty compile cache; the workspace's crates took
760 CPU-seconds (eight times less) and the dependencies 593.

**Where the test time goes.** `cargo test` runs the test programs one after another: 317 s of
test time locally, 270 s on the Linux runner. cargo-nextest runs them side by side, one process
per test: every test in 121 s locally (the `ci` profile; 141 s with the release profile; the
`ci` profile's code runs as fast, it is optimised the same). The slowest test programs (ci
profile, locally; every one is a single test over a recorded corpus):

| Test program | Seconds | Tier |
|---|---|---|
| `hydo-control::corpus` (the controller on the identity corpus) | 119 | full |
| `hydo-world::xcheck` (about 80 steps against the Go program's transcript) | 57 | full |
| `hydo::equivalence` (`cli_matches_golden`, 148 invocations) | 28 | fast |
| `hydo-grpc::streams` (request streams like Go's) | 23 | full |
| `hydo-control::features` | 20 | full |
| `hydo-build::equivalence` (7,430 runs against the Python's answers) | 18 | full |
| `hydo-kvm::engine` | 16 | fast |
| `hydo::agents_equivalence` | 15 | fast |
| `hydo-manifest::equivalence` | 13 | full |
| `hydo-composer::equivalence` (59,262 cases) | 12 | full |
| `hydo-control::m123` | 11 | full |
| `hydo-control::corpus_net` | 10 | full |
| `hydo-explore::corpus` | 8 | full |

The documentation examples cost 2 to 3 seconds per run in CI: 25 libraries had none (rustdoc
still parsed each crate to find out), one had one (`hydo-history`, a `no_run` example). The 24
libraries without examples have `doctest = false` now; `hydo-history`'s example runs in the full
tier.

**After.** The per-push tier compiles with the `ci` profile through sccache and runs the `fast`
tests with cargo-nextest: on the branch `ci-speed`, the Linux `rust` job took 6.5 minutes with no cache at all (compiling 4 min 48 s, the 458 tests 14 s) and then 2.8 to 3.3 minutes with warm caches (clippy 14 to 19 s, compiling 2 min 1 s to 2 min 18 s, the tests 9 to 11 s); the macOS job took 8.2 minutes on its first run and 4.3 minutes with warm caches (compiling 2 min 59 s, the 457 tests 24 s). A whole per-push run (the four jobs side by side) took 4 min 20 s, against 24 minutes before (runs 42386412977, 42388923392 and 42390523809). With warm caches almost all of the remaining compile time is the test programs and programs themselves, which no cache holds (below); the dependencies come from `Swatinem/rust-cache` and the workspace's libraries from sccache (18 of 25 compiles from the cache on Linux; the 7 others are `hydo-image`, whose build script embeds the commit, and libraries that depend on it). A change confined to a few crates compiles and tests only those and their dependents.

Locally, in a worktree with an empty target directory and the shared cache warm (the
dependencies built once by any worktree), building every test program takes 87 s; in the same
worktree after `cargo clean` it takes 46 s (its own crates come from the cache too); with no
cache at all 132 s; the release profile's 534 s are what a fresh worktree paid before. The
`fast` tier then runs 457 tests in 25 s.

## Test tiers

Every test binary of the workspace is in `ci/test-tiers.txt`, as cargo-nextest names it
(`<package>` for a library's unit tests, `<package>::bin/<name>`, `<package>::<test file>`,
`<package>::doctest` for a library's documentation examples), with one of two tiers. In a crate
whose integration tests are one program (below, "One test program per crate") the line
`<package>::<test file>` names that file's module of the program `it`: its tests are
`<test file>::<test>`, and `ci/tier-filter.sh` turns the line into the filter
`(binary_id(<package>::it) & test(/^<test file>::/))`. The tiers are:

- `fast`: run by the per-push tier (for the affected packages, below) and by the full tier;
- `full`: run by the full tier, and by the per-push tier only when the push changes a file of the
  binary's own package. `full` is for the large equivalence corpora against recorded answers
  (thousands of cases, ten seconds or more each; the table above) and for documentation examples,
  which cargo-nextest does not run.

So every test runs in the full tier, and none can be in neither: `hydo-release`'s `test_tiers`
test (in the `fast` tier, and always run) fails when a test binary of the workspace has no line,
when a line names no binary, when a line is doubled or names another tier, and when a library
with `doctest = false` has a documentation example rustdoc would test; since the consolidation
also when a `tests/*.rs` is in no test program, when a `tests/main.rs` holds a test of its own,
and when a program or library with `test = false` has a unit test. A new test file is added as
`fast` unless it is such a corpus.

Which packages a push tests: `ci/affected-packages.sh BASE` (BASE is the push's previous commit
or the pull request's base) maps every changed file to the workspace member that owns it and
adds every workspace package that depends on that one (`cargo metadata`, normal, dev and build
dependencies); a file under a crate's `tests/` also selects the packages whose sources or tests
name that crate's directory. Any changed file outside the workspace members (the lock file,
`Cargo.toml`, `.cargo/`, docs, examples, the skill, scripts, workflows) selects every package,
because some test reads it (`public_content` reads every tracked file); so does a push whose base
is unknown (a new branch, a force push). `hydo-release` and `hydo-agents` are always added (the
tier check and the tracked-files check). `ci/affected-packages.sh --direct BASE` lists the
packages that own a changed file; their `full` binaries run too (`ci/tier-filter.sh fast
<package>...`). What the selection misses, the full tier finds within a day.

## One test program per crate

Cargo makes every `tests/<name>.rs` its own test program and every library and program its own
unit-test program, each compiled and linked against the crate and all its dependencies. Until
this change the workspace had 113 test programs (cargo-nextest's count), and a build of them
compiled the same support modules (hydo-control's scripted agent and engine, hydo-explore's
corpus support) once per program. Now it has 51:

- **One integration-test program per crate.** In every workspace crate with more than one
  `tests/<name>.rs` (18 crates, 61 files), the files are modules of one program, `it`:
  `tests/main.rs`, written by `ci/consolidate-tests.sh`, declares the shared support directories
  (`tests/<dir>/mod.rs`) once and every `tests/<name>.rs` as the module `<name>`; the crate's
  `Cargo.toml` has `autotests = false` and `[[test]] name = "it"`; a test is called
  `<name>::<test>`. The files keep their paths, so the goldens, fixtures, `include_str!` paths and
  the paths in comments stay as they are; in a folded file the line `mod <dir>;` naming a support
  directory became `use crate::<dir>;`, and nothing else changed. One file's tests:
  `cargo test -p <package> --test it <name>::`, or `cargo nextest run -p <package> -E
  'test(/^<name>::/)'`. Re-record commands that are part of a golden's content (hydo-control's
  and hydo-explore's golden headers, the `$comment` of the generated project schema) still say
  `--test <name>` and were left as they are; Cargo answers such a command with the list of test
  targets, `it` among them.
- **No test program for a program or library without a unit test** (`test = false` on its
  `[[bin]]` or `[lib]`): 18 programs and the libraries `hydo-results`, `hydo-schedule` and
  `hydo-schema-compat`. Cargo still builds the programs the integration tests run.
- **What stays its own program, and why:** hydo-explore's `fake_engine` (`harness = false`: the
  test program is also the scripted engine it starts, with its own `main`); hydo-grpc-gen's
  `client` (it sets `HYDO_SEED`, `HYDO_OUTCOMES`, `HYDO_ACKED` and `HYDO_GRPC_CONSOLE` in the
  process environment, under its own lock; under `cargo test` the crate's other tests would run as
  threads of the same process and see them); and the crates with a single test file (hydo-config,
  hydo-engine, hydo-errors, hydo-grpc, hydo-hvm, hydo-schedule, hydo-world), which are one program
  already. Under `cargo test` the folded files' tests now run as threads of one process
  (cargo-nextest runs each test in a process of its own anyway). Checked for that: the tests that
  set the umask (hydo-build's `oci`, hydo's `agents_equivalence`, hydo-compose's and
  hydo-composer's `equivalence`) all set 022, the value the other tests assume; every temporary
  directory carries its file's own prefix; no other test changes process-wide state.
- **Adding a test file:** run `sh ci/consolidate-tests.sh` (idempotent; `--check` only reports what
  it would change and exits 1), then `cargo fmt`, and add the file's `<package>::<name>` line to
  `ci/test-tiers.txt`. With `autotests = false` Cargo would silently not compile a file that no
  program names, so `hydo-release`'s `test_tiers` fails while a `tests/*.rs` of a workspace crate
  is in no program, while a `tests/main.rs` holds a test of its own, and while a program or
  library with `test = false` has a unit test. A file that must stay its own program goes into the
  script's `SEPARATE` list with its reason.

Measured on this Mac on 2026-11-07, `cargo test --profile ci --workspace --no-run`, with the
shared compile cache warm, alternating before and after, two runs each after a priming run (load
averages of 20 to 220 from other agents, so the ranges are wide):

| | Before | After |
|---|---|---|
| Test programs (nextest) | 113 | 51 |
| Programs linked per build | 164 | 102 |
| Build with `target/ci` deleted | 60 to 71 s | 36 to 44 s |
| Rebuild after touching `hydo-manifest` (68 / 31 links) | 27 to 46 s | 16 to 27 s |
| Rebuild after touching `hydo-control` (32 / 15 links) | 23 to 32 s | 10 to 18 s |
| CPU time of the test-program compilations (`--timings`) | 531 to 601 s | 287 to 327 s |
| Time spent linking (all links, summed) | 23 to 28 s | 14 to 15 s |
| `cargo nextest run` of every test | 112, 113 s | 111 s (192 s once under load) |
| The `fast` tier | 35, 36 s | 39, 47 s |

Every test is kept: the 482 tests before are the 482 after (compared name by name, `<file>`
becoming the module `<file>::`), plus the two new checks of `test_tiers`. The test runs take as
long as before: they are bound by the slowest single test (hydo-control's corpus, about 110 s),
and the `fast` tier's few seconds more are within the load's noise. Most of the saving is
compile time, not link time: Apple's linker linked each program in about 0.2 s.

**The linker stays as it is.** On this Mac rustc links with the system linker (Apple's ld-1267,
"ld-prime"). Rust's own LLD (`rust-lld` as `ld64.lld`, passed to `cc` with `-fuse-ld`) was
slower on the same builds: 17 and 22 s of linking against 13 s, with the same wall times or
worse. On Linux, Rust 1.90 already links `x86_64-unknown-linux-gnu` programs (the CI runner's
and the bare-metal machine's tests) with its bundled LLD, and the release's static musl
programs with `rust-lld` (`.cargo/config.toml`). Linking is about 4 % of the test build's CPU
time, so no other linker was taken.

The release builds do not change: `hydo`, `hydo-world` and `hydo-grpc` built with `--release`
from main and from this change at one path (with one `hydo/COMMIT`, so that the compiled-in
commit is the same) have the same bytes, and the pinned recipe of the guest gRPC client
(`hydo/grpcgen/build-client.sh`, whose crate gained `test = false`) gives the pinned
`3926d137…` and `fd762fa0…`. The full tier (`cargo test --locked --release --workspace`) passes:
483 tests and 2 ignored in 52 programs (with the documentation example).

## The compile cache

User decision (2026-11-07): no cache is shared between machines, and no remote cache server or
bucket is used anywhere.

**Locally (this Mac and any developer machine):** `.cargo/config.toml` names
`.cargo/rustc-wrapper.sh` as Cargo's rustc wrapper, so every build in the repository and in each
of its git worktrees goes through it. With [sccache](https://github.com/mozilla/sccache) on PATH
it runs the compiler through sccache with hydo's own cache only: the git-ignored directory
`.sccache/` of the main checkout (a worktree finds the main checkout through its `.git` file, as
`git rev-parse --git-common-dir` does), a sccache server of its own (a port derived from that
directory), no configuration file and no remote backend, 8 GB at most (`HYDO_SCCACHE_SIZE`).
Other projects on the machine and a user's own sccache settings are untouched; nothing leaves
the machine. Without sccache, or with `HYDO_SCCACHE=0`, it runs the compiler itself: nothing
needs sccache. `.cargo/rustc-wrapper.sh --show-stats` (or `--zero-stats`, `--stop-server`) talks
to the repository's server. The pinned version is sccache 0.18.0 (from its GitHub release:
`sccache-v0.18.0-aarch64-apple-darwin.tar.gz`, sha256
`308184519b646f5125289e8515b36f6ca65a13a041923994aebe702348674e8e`; Linux musl
`45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89`), installed as one file on
PATH (`~/.cargo/bin/sccache` on this Mac). The directory is excluded from everything that copies
the tree: git (`.gitignore`), `dev.sh`'s rsync to the build machine; `hv/hvx/deploy-metal.sh`
copies named paths only, and the release scripts and `cargo package` take tracked files only.

What it shares, measured: sccache caches libraries only (Cargo's programs, test programs and
procedural macros are linked, which sccache does not cache), and its keys contain the absolute
path of the crate's directory (`CARGO_MANIFEST_DIR`, which `SCCACHE_BASEDIRS` does not normalise
for Rust). So between worktrees the third-party dependencies are shared (102 of the 145 cacheable
compiles of the workspace's tests), and a worktree's own crates are found again only in the same
worktree (after `cargo clean`, or when a build returns to a profile it left). A changed build
script value is a different key: a new commit's `BUILT_FROM_COMMIT` (hydo-image) was compiled
again, not taken from the cache.

**In CI:** the `rust` jobs of both workflows set `RUSTC_WRAPPER=sccache` with
`SCCACHE_GHA_ENABLED`: sccache 0.18.0 (installed by `mozilla-actions/sccache-action`, pinned by
commit) keeps its cache in this repository's GitHub Actions cache only, which no developer
machine reads or writes. `Swatinem/rust-cache` keeps the Cargo registry and the compiled
dependencies as before.

**Reproducibility.** The release profile never uses the cache: the wrapper runs the compiler
itself for every compile whose output directory is a `release` profile's
(`<target>[/<triple>]/release/...`), which covers the release tarballs (`build-release.sh`), the
pinned guest programs (`hydo-world`'s `build.sh`, `hydo/grpcgen/build-client.sh`, which also sets
its own `RUSTC_WRAPPER`, the agent's and the store reader's builds) and `cargo install`. Checked
in Milestone 171: `hydo`, `hydo-world` and `hydo-grpc` built with `--release` from main and from
this change at one path give the same bytes, and the pinned recipes give the pinned bytes
(`hydo-grpc` `3926d137…` and `fd762fa0…`, `hydo-world` `082592ae…`). The `ci` profile is only for
tests.

## Golden tests and their pinned inputs (Milestone 171)

Several equivalence tests pin the repository files they read with a sha256 (`I` lines of
`hydo-build`'s and `hydo-manifest`'s `answers.txt`, `input` lines of `hydo-explore`'s and
`hydo-control`'s goldens, `hydo-composer`'s plan folders, `hydo-errors`' known-errors files).
Until Milestone 171 a pin that differed failed the test on its own, so an edit that changes no
answer (a package renamed in a description of `hydo/dist/*/third-party.json`, which the builder
refuses at the top level whatever the description says) failed them until someone edited the
hashes. Now the answers decide: a pin that differs is printed, and the test fails only when an
answer differs, naming the changed inputs in the failure. The input set itself is still pinned:
a new, missing or reordered input of `hydo-explore`'s goldens fails, and a vanished file
changes its answers. `hydo-compose`'s `pycheck` already skipped changed Compose files (its
answers are Python's and cannot be re-recorded) and `hydo`'s `agents_equivalence` only reported
the skill folder's hash; both are unchanged. Checked: a description edited in
`hydo/dist/linux/third-party.json` passes `hydo-build::equivalence` and
`hydo-manifest::equivalence` (each prints the changed file); a comment added to
`hydo/onboard/examples/redis/hydo.yaml` passes `hydo-explore::corpus`, `hydo-explore::identity`
and `hydo-control::corpus_net`; and a changed readiness command in the same file fails
`hydo-build::equivalence` (7 of 7,430 answers differ) and `hydo-manifest::equivalence`, naming
the file.

## What does not run, and why

Nothing in CI boots a guest: a GitHub runner has neither the hvx kernel module (`/dev/hvx`)
nor access to Hypervisor.framework. (A Linux runner does have `/dev/kvm`, so the KVM engine of
Milestone 159 could boot guests there; that is not part of this minimal workflow.) So `hv/hvx/deploy-metal.sh`, the pinned reproducers, the
flagship verifies, the hand-written C module tests, the lock-step and agent boot checks and the
release builds stay on the bare-metal machine and the Mac (docs/RELEASING.md, AGENTS.md).

The tests that do not run in full on a runner, each with the reason it gives; each tier prints the run-time reasons (the per-push tier at the end of its nextest run, `.config/nextest.toml`; the full tier in its last step):

- `hydo-errors`' `recorded_files` prints `SKIPPED: set HYDO_ERRLOG_CORPUS to a copy of the M94
  results` and passes: it recomputes the files of M94's real runs, which are not in the
  repository. The other `hydo-errors` equivalence tests run.
- `hydo-agents`' `public_content` prints `public_content: no AGENTS.local.md; only the generic
  rules are checked`: the list of concrete values that must never be tracked is git-ignored, so
  a runner checks the generic patterns only (remote-shell commands, addresses, the hosting
  company's words). It must pass, and does.
- `hydo-read`'s five store tests (MySQL, PostgreSQL, Kafka, Redis, etcd) each print `HYDO_TEST_<STORE>
  not set (run native-test.sh): skipped`: they need live servers, which `native-test.sh`
  starts on a machine with the stores. The reader's unit tests and its check tests run.
- Ignored on purpose (run by hand, never in CI): `hydo-history`'s `dump_cases` (it writes the
  generated corpus for a recording, `HYDO_HIST_DUMP`) and `hydo-compose`'s registry test (it
  records `tests/fixtures/registry` from the live registries).
- Platform-only tests run on their own runner only: `hydo-hvx`'s `options.rs` and the guest
  agent's native tests on Linux, `hydo-hvm`'s `c_layout.rs` (the Mac engine's Hypervisor.framework constants and
  layouts against the deleted C engine's recorded answers) on macOS.
- Optional extra comparisons that need files from git history are not run:
  `HYDO_HVX_C_SRC` and `HYDO_HVM_C_SRC` compare the engines with the deleted C sources
  (`git show`); without them the tests compare with the C's recorded answers, which they do.
- The SDKs skip what their language cannot express, and say so: Go skips the one `emit_line`
  case (the Go SDK has no `emit_line`); Python skips the two float32 cases; JavaScript skips
  the two float32 cases and two invalid-UTF-8 history cases. With Go 1.27 or later the Go SDK
  would also skip five invalid-UTF-8 cases (Go 1.27 writes a raw U+FFFD where the guest images'
  Go 1.26.5 writes `�`), which is why the job pins Go 1.26.5. The Rust SDKs' doc examples
  are marked `ignore` (they are snippets for guest programs), so four doc tests show as ignored.

## Tests changed so that they do not depend on the machine (Milestone 167)

- The `hydo` command's `cli_matches_golden` (package `hydo`, directory `hydo/crates/hydo-cli`) recorded the answers of a machine without the hvx module,
  KVM, `/dev/hvx`, the udev rule or a `debugfs` on PATH. A Linux runner has `kvm` loaded
  (`hydo module load --dry-run` then planned `sudo rmmod kvm`) and Ubuntu's `debugfs`, which
  changed how a Mac case reads an image; the bare-metal machine has hvx loaded. `hydo` now reads
  the host's module state (`/proc/modules`, `/proc/locks`, `/sys/module`, `/dev`,
  `/etc/udev/rules.d`) under `HYDO_CLI_HOST_ROOT` (a developer switch, `/` when unset; printed
  paths and writes are unchanged), and the test sets an empty directory there and a PATH whose
  directories holding a `debugfs` are replaced by symlink copies without it. No recorded answer
  changed.
- The `hydo` command's `agents_equivalence`, `hydo-compose`'s `equivalence` and `hydo-build`'s `oci`
  record file modes. They now set the process's umask to 022, and the first two copy their
  inputs (the skill folder, the Compose fixtures) with the modes git gives a checkout under
  umask 022, so a umask of 002 (common for users on Linux) or a checkout made with it gives the
  same answers. A runner's umask is 022, so these passed there before; on the bare-metal
  machine they did not (docs/HISTORY.md, the M158 addendum's checks).
- The JavaScript and Python SDKs' shared-case children read a monotonic clock that advances by
  one nanosecond per reading. Every shared case expects `elapsed_ms` 0, which the guest's
  deterministic clock gives; on a busy runner one millisecond passed between `begin` and `done`
  once. The Go and Rust SDKs read the system clock and cannot be given another without changing
  the SDKs, whose bytes are inside recorded images; their calls take microseconds, but on a very
  slow machine the same can happen there. The unit tests still test `eventually` against the
  real clock.


## Running the same checks locally

On a Mac or a Linux machine with the toolchain of `rust-toolchain.toml`, and cargo-nextest
0.9.148 (`cargo-nextest-0.9.148-universal-apple-darwin.tar.gz`, sha256
`6c23b7fb4ca82c571fc8dfedbab45bfa773d45918f933170c85d21cd7f4b852b`; Linux
`cargo-nextest-0.9.148-x86_64-unknown-linux-gnu.tar.gz`, sha256
`223ba936714cc861a9cdedad5a8eb664dfdd69d6fbdf6cea6aa8184f523290dc`; the program `cargo-nextest`
on PATH) and, optionally, sccache 0.18.0 on PATH:

```sh
cargo fmt --all --check
sh ci/consolidate-tests.sh --check     # every crate's integration tests are one program
cargo clippy --locked --workspace --all-targets -- -D warnings
# the per-push tier's tests for every package (about a minute once built)
cargo nextest run --locked --cargo-profile ci --workspace -E "$(sh ci/tier-filter.sh fast)"
# the full tier, as before a release (docs/RELEASING.md step 1)
cargo test --locked --release --workspace
```

`sh ci/affected-packages.sh main` prints the packages a change since `main` can affect (or
`ALL`); pass them as `-p <package>` to the nextest line, and `sh ci/affected-packages.sh --direct
main` to `ci/tier-filter.sh fast` to include their own corpora, as CI does.
`cargo nextest run --cargo-profile ci --workspace` (no filter) runs every test but the
documentation example in about two minutes.

The SDKs: `cargo test` in `sdk/rust` and `sdk/rust-test`, `GOTOOLCHAIN=go1.26.5 go test ./...`
in `sdk/go`, `python3 -m unittest discover -s sdk/python/tests`, and
`node --test` in `sdk/js`.
