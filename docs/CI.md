# Continuous integration

`.github/workflows/ci.yml` runs on every push and every pull request (Milestone 167). It needs
no secret, reads the repository only (`permissions: contents: read`, the checkout keeps no
credentials), pins every action by commit, and runs every step with `bash -eo pipefail`.

## What runs

| Job | Runner | What it runs | Time |
|---|---|---|---|
| `rust (ubuntu-24.04)` | GitHub's Linux x86_64 runner | `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --release --workspace --no-fail-fast`, then a step that prints what the tests skipped | about 15 minutes (25 without a warm cache) |
| `rust (macos-15)` | GitHub's macOS Apple Silicon runner | the same | about 25 minutes |
| `guest programs (ubuntu-24.04)` | Linux | the guest agent (`hydo/crates/hydo-agent`: its native tests, then the agent as PID 1 of its own PID and mount namespace, which needs `sudo -n` and runs because the runner has password-less sudo) and the store reader (`hydo/crates/hydo-read`); both keep their own Cargo workspaces and lock files | under 1 minute |
| `sdks` | Linux | the Rust SDK (`sdk/rust`, the reference that writes `sdk/cases/`), the Rust test SDK (`sdk/rust-test`), the Go SDK (`hydo/stack/hydoassert`, `hydo/stack/hydohist`) with Go 1.26.5, the Python SDK with Python 3.9 and 3.13, the JavaScript SDK with Node 22 | about 1.5 minutes |

The three steps of the `rust` jobs are the checks docs/RELEASING.md step 1 asks for before a
release. The skill check (`hydo-agents`' `skill_check`) also runs the Agent Skills reference
validator: the job installs `skills-ref` 0.1.1 from PyPI into a virtual environment and names
its program (`agentskills`) in `HYDO_SKILLS_REF`.

**Why a run takes 15 to 25 minutes.** Almost all of it is compiling the workspace's 26 crates
and their 126 test programs with the release profile (thin LTO, one codegen unit) on the
runner's 3 or 4 cores: about 11 minutes on Linux and 18 on macOS with the dependencies from the
cache. `Swatinem/rust-cache` keeps the Cargo registry and the compiled
dependencies (they compile in seconds from the cache), but not the workspace's own crates: a
fresh checkout's source files are newer than any cached build, so Cargo compiles them again.
The tests themselves take 3 (Linux) to 5 (macOS) minutes: 382 pass on Linux and 381 on macOS, 0 fail, 2 are ignored (below). The `rust` job of a commit that changes no Rust is as
long as one that does.

## What does not run, and why

Nothing in CI boots a guest: a GitHub runner has neither the hvx kernel module (`/dev/hvx`)
nor access to Hypervisor.framework. So `hv/hvx/deploy-metal.sh`, the pinned reproducers, the
flagship verifies, the hand-written C module tests, the lock-step and agent boot checks and the
release builds stay on the bare-metal machine and the Mac (docs/RELEASING.md, AGENTS.md).

The tests that do not run in full on a runner, each with the reason it gives; the last step of
the `rust` jobs prints the run-time reasons:

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

- `hydo-cli`'s `cli_matches_golden` recorded the answers of a machine without the hvx module,
  KVM, `/dev/hvx`, the udev rule or a `debugfs` on PATH. A Linux runner has `kvm` loaded
  (`hydo module load --dry-run` then planned `sudo rmmod kvm`) and Ubuntu's `debugfs`, which
  changed how a Mac case reads an image; the bare-metal machine has hvx loaded. `hydo` now reads
  the host's module state (`/proc/modules`, `/proc/locks`, `/sys/module`, `/dev`,
  `/etc/udev/rules.d`) under `HYDO_CLI_HOST_ROOT` (a developer switch, `/` when unset; printed
  paths and writes are unchanged), and the test sets an empty directory there and a PATH whose
  directories holding a `debugfs` are replaced by symlink copies without it. No recorded answer
  changed.
- `hydo-cli`'s `agents_equivalence`, `hydo-compose`'s `equivalence` and `hydo-build`'s `oci`
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

On a Mac or a Linux machine with the toolchain of `rust-toolchain.toml`:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --release --workspace
```

The SDKs: `cargo test` in `sdk/rust` and `sdk/rust-test`, `GOTOOLCHAIN=go1.26.5 go test ./...`
in `hydo/stack/hydoassert`, `python3 -m unittest discover -s sdk/python/tests`, and
`node --test` in `sdk/js`.
