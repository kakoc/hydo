# Contributing to hydo

This page says how to build hydo from this repository, how to test it, and which rules every
change must keep. `docs/ARCHITECTURE.md` explains the parts. `AGENTS.md` holds the full rules and
the project's current state. People and coding agents read the same file.

## What you need

- An Apple Silicon Mac, or x86_64 Linux.
- Rust 1.90.0. `rust-toolchain.toml` selects it when you use rustup.
- cargo-nextest 0.9.148 for the fast test tier. `docs/CI.md` gives its download and sha256.
- Optional: sccache 0.18.0 on PATH. `.cargo/rustc-wrapper.sh` then caches compiles in
  `.sccache/` of the main checkout.

## Build the hydo command

```sh
cargo build --release -p hydo
target/release/hydo --version
target/release/hydo setup
```

The package is named `hydo`; its directory is `hydo/crates/hydo-cli`. A checkout has none of a
release's files: the guest kernels, the engine programs, and the guest tools. `hydo setup`
downloads the release of the tree's version into `~/.hydo/versions/<version>` once. The `hydo` of
the tree then runs over those files.

The engines need more:

- The kernel module needs x86_64 Linux with Intel VT-x and the running kernel's headers.
  `make -C hv/hvx` builds `hvx.ko`.
- `hv/hvx/deploy-metal.sh` runs the module's regression suite on such a machine from another
  computer. It reaches the machine through a file `machines.local.sh` that you write and that git
  ignores. The comment at the top of `machines.sh` says what the file defines.
- The Mac engine needs an Apple Silicon Mac. `cargo build --release -p hydo-hvm` builds it, and
  hydo signs it with `hv/hvm/hv.entitlements` before it runs it.
- Guest images are reproducible only with the pinned toolchain in `docs/BUILD-PINS.md`.

## Test

Run these checks before you push. CI runs the same checks (`docs/CI.md`).

```sh
cargo fmt --all --check
sh ci/consolidate-tests.sh --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --cargo-profile ci --workspace -E "$(sh ci/tier-filter.sh fast)"
sh ci/vale.sh
```

The tests have two tiers. `ci/test-tiers.txt` puts every test program in one of them:

- The fast tier runs on every push. It skips the large recorded corpora.
- The full tier runs every test in release mode, every night and before a release:

  ```sh
  cargo test --locked --release --workspace
  ```

Each crate has one integration-test program, `it`. To run one test file of a crate, give its
module name, for example `cargo test -p hydo-agents --test it skill_check::`.

No test boots a guest. Changes to an engine, the host controller, the agent, or a guest kernel
also need runs on real machines: `hv/hvx/deploy-metal.sh` for hvx, and `hydo-bench rebaseline`
for the recorded example values (`docs/REBASELINE.md` "How to re-baseline").

## Rules every change must keep

`AGENTS.md` "Safety and coordination rules" has the full text. These rules matter most:

- Keep every recorded fingerprint. A new feature must be opt-in and inert when it is off. A change
  that moves a recorded fingerprint must also raise the fingerprint epoch
  (`hydo_image::EPOCH`) and add a section to `docs/REBASELINE.md`.
- Change a versioned format only by additions. The schema tests compare each format with its
  released copy in `hydo/schema/released/`.
- Write code in Rust. The kernel module stays in C. A rewrite must give the same bytes and
  fingerprints as the code it replaces.
- Do not write prose specifications. Tests and their recorded examples are the specification.
- In tracked files, call the two project machines only `the bare-metal machine` and `the build
  machine`. Write no address, login, or host name. The test
  `cargo test -p hydo-agents --test it public_content::` checks this.
- Do not present hydo in relation to another product in its space. Research notes can cite
  published work factually.
- Do not put links to Claude sessions in commit messages or pull-request descriptions.

## Write documentation

Write every document to the standard in `docs/STYLE.md`. The skill `skills/hydo-docs/` gives the
same rules in brief. A coding agent can load it. Check your text with Vale:

```sh
sh ci/vale.sh
```

The command must print `ci/vale.sh: no findings`. To check another file, give its path as an
argument.

## Record your work

The project records each piece of work as a milestone:

1. Add a milestone section with the details and the evidence to the end of `docs/HISTORY.md`.
2. Add a one-line entry to the milestone index in `AGENTS.md`.
3. Update the current state in `AGENTS.md`, and its open items if something stays open.
