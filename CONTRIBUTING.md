# Contributing to hydo

This page says how to build hydo from this repository, how to test it, and how to submit a
change. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains the parts.
[AGENTS.md](AGENTS.md) is the maintainers' notes: the full rules, the project's machines, and its
current state.

## What you need

- An Apple Silicon Mac, or x86_64 Linux.
- Rust 1.90.0. When you use rustup, [rust-toolchain.toml](rust-toolchain.toml) selects it.
- cargo-nextest 0.9.148 for the fast test tier. Install it with
  `cargo install cargo-nextest --version 0.9.148 --locked`.
- The documentation check needs network access, `curl`, and `unzip`.
- The SDK tests need Go 1.26.5, Python 3.9 or later, and Node.js 22.
- Optional: sccache 0.18.0 on PATH. The build then keeps a compile cache in `.sccache/`.

## Build the hydo command

```sh
cargo build --release -p hydo
target/release/hydo --version
target/release/hydo setup
```

The package is named `hydo`; its directory is [hydo/crates/hydo-cli](hydo/crates/hydo-cli). A
checkout has none of a release's files: the guest kernels, the engine programs, and the guest
tools. `hydo setup` downloads the published release of the tree's version into
`~/.hydo/versions/<version>`, once. The `hydo` of the tree then boots guests with those files.
`target/release/hydo setup --check` and `target/release/hydo doctor` report whether that step is
still to do.

Without release files, everything except booting a guest works: the build, `hydo build --check`,
and every test of the fast and full tiers. When the tree's version is not published yet, a
checkout cannot boot guests; maintainers then build a release from the tree
([docs/RELEASING.md](docs/RELEASING.md), section 2).

## Build the engines

The kernel module needs x86_64 Linux with an Intel CPU, VT-x, a C compiler, `make`, and the
running kernel's headers. Build it from this tree:

```sh
make -C hv/hvx
```

`hydo module install` and `hydo module uninstall` load and unload a release's module, and they
take the module lock, so that no run loses its module. They build the release's copy of
`hvx.c`, not this tree's. To load the module you built, take the same lock yourself. Loading it
needs `kvm_intel` and `kvm` unloaded, which stops every virtual machine that runs under KVM:

```sh
sudo rmmod kvm_intel kvm
flock ~/.hvx-module.lock sudo insmod hv/hvx/hvx.ko
```

`flock ~/.hvx-module.lock sudo rmmod hvx` unloads it again. Then `sudo modprobe kvm_intel` brings
KVM back.

The Mac engine needs an Apple Silicon Mac. `cargo build --release -p hydo-hvm` builds it, and hydo
signs it with [hv/hvm/hv.entitlements](hv/hvm/hv.entitlements) before it runs it. The KVM engine
is part of the `hydo` program on Linux.

## Reproducible builds

`hydo build` makes the same image bytes on any machine, with no extra tools. The guest kernels,
the pinned guest programs, and the hand-made images in [hv/hvx/images/](hv/hvx/images/) are
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
can move recorded fingerprints, and only runs on real machines show that. Two maintainers' tools
make those runs on the project's test machine, an Intel server with the module loaded:

- [hv/hvx/deploy-metal.sh](hv/hvx/deploy-metal.sh) runs the module's regression suite. It needs
  `machines.local.sh`, a file that git ignores and that says how to reach the machine
  ([machines.sh](machines.sh) describes it).
- `hydo-bench rebaseline` runs every recorded example and compares the values
  ([docs/REBASELINE.md](docs/REBASELINE.md), "How to re-baseline"). It needs an unpacked release
  of the platform:

  ```sh
  target/release/hydo-bench rebaseline --release <unpacked release> --work <work folder> --check
  ```

If you do not have such a machine, say in the pull request that your change touches an engine or
the guest. A maintainer then runs the suite.

## Rules every change keeps

- Keep recorded fingerprints. A new feature must be opt-in and must change nothing when it is off.
  A maintainer checks the fingerprints on the pull request. A change that moves them on purpose
  raises the fingerprint epoch, which [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains.
- Change a format by changing its tests and its example files in
  [hydo/schema/examples/](hydo/schema/examples/), by additions only. The schema tests fail when a
  format changes by more than additions since its released copy in
  [hydo/schema/released/](hydo/schema/released/).
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
3. Your contribution is licensed under the license of each file that you change. A new file is
   licensed under Apache-2.0, except in [hv/hvx/](hv/hvx/), where
   [hv/hvx/LICENSE.md](hv/hvx/LICENSE.md) says which license applies. `hvx.c` is licensed under
   GPL-2.0-only.

Maintainers record each merged piece of work as a milestone; AGENTS.md "How to record new work"
says how.
