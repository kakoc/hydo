# Contributing to hydo

This page says how to build hydo from this repository, how to test it, and how to submit a
change. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains the parts.
[AGENTS.md](AGENTS.md) holds the project's full rules and its current state.

## What you need

- An Apple Silicon Mac, or x86_64 Linux.
- Rust 1.90.0. When you use rustup, [rust-toolchain.toml](rust-toolchain.toml) selects it.
- cargo-nextest 0.9.148 for the fast test tier. Install it with
  `cargo install cargo-nextest --version 0.9.148 --locked`, or download the pinned build that
  [docs/CI.md](docs/CI.md) names.
- Optional: sccache 0.18.0 on PATH. `.cargo/rustc-wrapper.sh` then keeps a compile cache in
  `.sccache/` of the main checkout. The main checkout is the first clone of the repository, as
  opposed to the extra working trees that `git worktree add` makes; all of them share the cache.

## Build the hydo command

```sh
cargo build --release -p hydo
target/release/hydo --version
target/release/hydo setup
```

The package is named `hydo`; its directory is [hydo/crates/hydo-cli](hydo/crates/hydo-cli). A
checkout has none of a release's files: the guest kernels, the engine programs, and the guest
tools. `hydo setup` downloads the published release of the tree's version into
`~/.hydo/versions/<version>`, once. The `hydo` of the tree then runs over those files.
`target/release/hydo setup --check` and `target/release/hydo doctor` report whether that step is
still to do.

If the tree's version has no published release yet, `hydo setup` stops, because there is nothing
to download. Then build a release from the tree with `hydo/dist/linux/build-release.sh` or
`hydo/dist/mac/build-release.sh` ([docs/RELEASING.md](docs/RELEASING.md), section 2), and use the
`hydo` inside it.

## Build and load the kernel module

The kernel module needs x86_64 Linux with an Intel CPU, VT-x, and the running kernel's headers.

```sh
make -C hv/hvx
```

Loading `hvx.ko` needs the kernel modules `kvm_intel` and `kvm` unloaded. That stops every
virtual machine that runs under KVM on the machine:

```sh
sudo rmmod kvm_intel kvm
sudo insmod hv/hvx/hvx.ko
```

`sudo rmmod hvx` unloads it again. Then `sudo modprobe kvm_intel` brings KVM back.

Optional: [hv/hvx/deploy-metal.sh](hv/hvx/deploy-metal.sh) runs the module's regression suite on
a remote Linux machine. It reaches the machine through a file `machines.local.sh` that you write
and that git ignores. The comment at the top of [machines.sh](machines.sh) says what the file
defines.

The Mac engine needs an Apple Silicon Mac. `cargo build --release -p hydo-hvm` builds it, and hydo
signs it with [hv/hvm/hv.entitlements](hv/hvm/hv.entitlements) before it runs it.

`hydo build` makes the same image bytes on any machine without extra tools. The guest kernels, the
pinned guest programs, and the hand-made images in [hv/hvx/images/](hv/hvx/images/) are
reproducible only with the pinned toolchain in [docs/BUILD-PINS.md](docs/BUILD-PINS.md).

## Test

Run these checks before you push. CI runs the same checks ([docs/CI.md](docs/CI.md)).

```sh
cargo fmt --all --check
sh ci/consolidate-tests.sh --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --cargo-profile ci --workspace -E "$(sh ci/tier-filter.sh fast)"
sh ci/vale.sh
```

`ci/consolidate-tests.sh --check` checks that each crate's integration-test files are modules of
one test program, `it`. If you add a file `tests/<name>.rs`, run `sh ci/consolidate-tests.sh` and
`cargo fmt`. To run one test file, give its module name, for example
`cargo test -p hydo-agents --test it skill_check::`.

The tests have two tiers. [ci/test-tiers.txt](ci/test-tiers.txt) puts every test program in one:

- The fast tier runs on every push. It skips the large recorded corpora.
- The full tier runs every test in release mode, every night and before a release:
  `cargo test --locked --release --workspace`.

Some parts keep their own Cargo workspace or language. Test them when you change them:

| Part | Command, in that folder |
|---|---|
| [hydo/crates/hydo-agent](hydo/crates/hydo-agent) | `cargo test --locked` |
| [hydo/crates/hydo-read](hydo/crates/hydo-read), [hydo/crates/hydo-sql](hydo/crates/hydo-sql) | `cargo test --locked` |
| [sdk/rust](sdk/rust), [sdk/rust-test](sdk/rust-test) | `cargo test` |
| [sdk/go](sdk/go) | `GOTOOLCHAIN=go1.26.5 go test ./...` |
| [sdk/python](sdk/python) | `python3 -m unittest discover -s tests` |
| [sdk/js](sdk/js) | `node --test` |

No test boots a guest. A change to an engine, the host controller, the agent, or a guest kernel
also needs runs on real machines. Run `hv/hvx/deploy-metal.sh` for hvx, and `hydo-bench rebaseline`
for the recorded example values ([docs/REBASELINE.md](docs/REBASELINE.md), "How to re-baseline").

## Rules every change keeps

- Keep recorded fingerprints. A new feature must be opt-in and must change nothing when it is off.
  A change that moves recorded fingerprints on purpose raises the fingerprint epoch, which
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains.
- Change a versioned format only by additions. The schema tests compare each format with its
  released copy in [hydo/schema/released/](hydo/schema/released/).
- Write hydo's own programs in Rust. The kernel module stays in C, and each SDK is in its own
  language. A rewrite must give the same bytes and fingerprints as the code it replaces.
- Tests are the specification ([docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), "Tests are the
  specification").
- Do not describe hydo in relation to other testing products. For example, do not write that hydo
  works like product X. Research notes can cite published work.
- Commit messages and pull-request descriptions contain no links to AI-assistant sessions.

## Write documentation

Write every document to the standard in [docs/STYLE.md](docs/STYLE.md). The skill
[skills/hydo-docs/](skills/hydo-docs/SKILL.md) gives the same rules in brief. A coding agent can
load it. Check your text with Vale:

```sh
sh ci/vale.sh
```

The command must print `ci/vale.sh: no findings`. To check another file, give its path as an
argument.

## Submit a change

1. Open a pull request against `main` on https://github.com/kakoc/hydo.
2. Make sure that CI passes. The `ci` workflow runs on every push, and `docs` runs when a document
   changes.
3. Your contribution is licensed under the license of each file that you change. Most files are
   licensed under Apache-2.0, and [hv/hvx/hvx.c](hv/hvx/hvx.c) under GPL-2.0-only. The header `hvx_abi.h` is
   GPL-2.0-only WITH Linux-syscall-note, or Apache-2.0.

## For maintainers: record the work

Maintainers record each piece of work as a milestone:

1. Add a milestone section with the details and the evidence to the end of
   [docs/HISTORY.md](docs/HISTORY.md).
2. Add a one-line entry to the milestone index in [AGENTS.md](AGENTS.md).
3. Update the current state in AGENTS.md, and its open items if something stays open.

The machine rules in AGENTS.md "Safety and coordination rules" apply to maintainers who use the
project's own machines.
