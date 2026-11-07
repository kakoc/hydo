# Publishing the hydo command

The `hydo` command is published to crates.io, so that `cargo install hydo` installs it
(Milestone 169). Nothing here is done automatically, and nothing is published until the user
decides to; the crates' metadata, the check and the dry run below are prepared in the tree. The
assertion SDKs have their own publishing steps.

## What is published

The package `hydo` (its directory is `hydo/crates/hydo-cli`, whose package was named
`hydo-cli` before Milestone 169) and the 19 workspace crates it depends on, each under its own
name. All 20 names were free on crates.io on 2026-11-07:

| crate | what it is |
|---|---|
| `hydo` | the `hydo` command (the binary `hydo`) |
| `hydo-config` | every environment variable hydo reads |
| `hydo-schedule` | the schedule language |
| `hydo-agent-proto` | the protocol between hydo and the guest agent |
| `hydo-manifest` | the project format (`hydo.yaml`, the `hydo/` folder) |
| `hydo-image` | the image records and how a run finds its guest kernel |
| `hydo-results` | the results format |
| `hydo-history` | the history checkers |
| `hydo-engine-proto` | the calls between hydo and its engines |
| `hydo-hvx` | the hvx engine (Linux, `/dev/hvx`) |
| `hydo-hvm` | the Mac engine (Hypervisor.framework) |
| `hydo-kvm` | the KVM engine (Linux, `/dev/kvm`) |
| `hydo-engine` | one interface over the engines |
| `hydo-composer` | the test composer, host side |
| `hydo-errors` | error logs as findings |
| `hydo-control` | the host controller of agent images |
| `hydo-explore` | the explorer |
| `hydo-agents` | `hydo guide`, `hydo skill`, `hydo init` (the onboarding skill is built in) |
| `hydo-compose` | the Docker Compose importer |
| `hydo-build` | the image builder |

Every other workspace crate has `publish = false`: the development tools (`hydo-bench`,
`hydo-tools`, `hydo-release`, `hydo-schema-compat`), the gRPC driver generator
(`hydo-grpc-gen`, shipped in the Linux tarball) and the guest programs. The crates outside the
workspace are not published either (`hydo-agent`, `hydo-read`: `publish = false`), except the
Rust SDK, which has its own steps.

**Why every crate under its own name, and not one merged crate.** crates.io builds a package
only from what is inside its directory, and a package's path dependencies must be published
crates too, so the choice is between publishing the 20 crates as they are and merging them into
fewer crates first. Publishing them as they are needs only metadata (a version next to each
path, the license, the repository, an `include` list) and changes no code, no program's output
and no recorded value. Merging would move every module of 19 crates, would change the crate
boundaries that keep the formats separate (each format crate is checked by its own schema
test), and would buy only a shorter publishing list; it would also have to be redone or kept
up whenever a crate is added. The cost of the chosen layout is 20 names on crates.io that are
not meant to be used on their own (their descriptions say what each is), and crates.io's limit
on new crates (below).

**The guest programs are not built by cargo.** `hydo-agent`, `hydo-world`, `hydo-read` and
`hydo-grpc` are inside guest images, are built by their own scripts to pinned bytes, and come
from the version's release archive like the guest kernels; no published crate depends on them
(the check below fails if one does).

## What `cargo install hydo` gives

`cargo install hydo` builds the `hydo` program only. Everything else a hydo installation has is
not built by cargo: the guest kernels and initramfs, the guest tools (busybox, mke2fs, the
iproute2 and iptables files), the guest programs, the examples, and the run engine as a program
of its own (on a Mac `bin/hydo-hvm`, which must carry the hypervisor entitlement; on Linux
`bin/hydo-hvx`, run under sudo when the user cannot open `/dev/hvx`, and `bin/hydo-kvm`). They
come from the GitHub release of the same version: `hydo setup --get` downloads the version's one
archive, checks it by sha256, unpacks it into `$HYDO_HOME/versions/<version>` (default
`~/.hydo`), signs the Mac engine with the entitlement, and the installed `hydo` uses that
directory as its installation (Milestone 162's install cache, docs/design/install-cache.md).
The engine program is the archive's, not a second build by cargo: it is the same program as the
tarball's, already verified with the rest of the archive, and building it with cargo would add a
Linux-only or a Mac-only program to every `cargo install` and a second, unverified copy of the
engine. `hydo guide`, `hydo skill install` and `hydo init` work before anything is downloaded,
because the onboarding skill is built into the program.

## How the crates build from crates.io alone (Milestone 169)

A published crate holds only the files inside its directory, and its build may not reach the
repository. What did not, and how each is fixed:

- **Files read at compile time from outside the crate.** `hydo-cli`'s `setup.rs` compiled in
  `hydo/dist/<platform>/assets.sha256` and `guest-programs.sha256`, and `hydo-agents`' build
  script embedded `skills/hydo-onboard/` and `hydo/onboard/MANIFEST.md`. Each crate now reads
  them through links inside its directory (`hydo-cli/pins/<platform>/*.sha256`,
  `hydo-agents/embed/hydo-onboard` and `embed/MANIFEST.md`); git keeps the links, the tree
  builds the same bytes through them, and `cargo package` copies the linked files' contents
  into the package. Each published crate also has a link `LICENSE -> ../../../LICENSE`.
- **The commit lookup.** `hydo-image`'s build script recorded `git rev-parse HEAD` (or the
  git-ignored `hydo/COMMIT`). In a package unpacked from crates.io the directory may sit inside
  another repository (a home directory kept in git), so a packaged crate (one with the
  `Cargo.toml.orig` cargo writes into every package) never asks git: it takes the commit from
  the `.cargo_vcs_info.json` that `cargo package` writes (`git.sha1`), and none when that file
  says the packaged tree was dirty. A crate published from a clean checkout of the release
  commit therefore records that commit, as the tarball's programs do; `hydo --version` (new in
  Milestone 169) prints it.
- **Versions and dependency cycles.** Every `[workspace.dependencies]` entry of a workspace
  crate has `version` next to `path` (crates.io needs a version for every dependency).
  Dev-dependencies on workspace crates are given by path only (`hydo-control`'s tests use
  `hydo-explore`, which depends on `hydo-control`): cargo leaves path-only dev-dependencies out
  of the published manifest, so the cycle is not published.
- **Only the sources are published.** Each crate's `include` lists `src/`, its build script, its
  `LICENSE` and the files it reads (`hydo-hvx`'s `pt/` for the optional coverage decoder). The
  tests and their recorded answers stay in the repository (cargo prints "ignoring test …" for
  each while it packs); the largest package is about 140 KiB compressed.
- **The installed layout.** A `hydo` installed by cargo lives in `~/.cargo/bin` with no release
  around it; the release root it uses is `$HYDO_HOME/versions/<its version>`.
- **Nothing calls a guest program, a tree path or a tool at build time.** The crates' build
  scripts are the three above (`hydo-agents`, `hydo-image`, and `hydo-hvx`'s, which compiles C
  only with the cargo feature `pt`).

`hydo-agent-proto` keeps its own version (0.1.0): the guest agent compiles it, so a version
change would move the agent's bytes and every image; at a later release it is published again
only when its version changed. Its `HYDO_VERSION` (in the host's messages about agent versions)
is its package version, so those messages name 0.1.0 until it is bumped.

The check `cargo test -p hydo-release --test crates` fails when any crate the `hydo` package
depends on is not publishable: missing metadata, a version other than `hydo/VERSION`, a
dependency without its version, a dev-dependency with a version, tests in the package, a file
it includes from outside its directory, a build script that names a path outside the crate (one
documented exception, `hydo-image`'s tree-only `../../COMMIT`), more than 10 MiB of files, a
guest program in the closure, a workspace crate outside the closure without `publish = false`,
or an order below that publishes a crate before its dependencies (`hydo/crates/hydo-release/src/publish.rs`).

## What is permanent

A published version can never be deleted or overwritten; it can only be *yanked*
(`cargo yank --version 0.1.0 <crate>`): new projects no longer resolve it, existing lock files
still download it. A crate name is taken for good once published. So run the dry run below on
the release commit before the first upload.

crates.io limits how fast new crates can be created (a burst of a few, then one about every ten
minutes, at the time of writing): publishing 20 new crates takes a few hours unless crates.io is
asked to raise the limit for the account first (help@crates.io). New versions of existing
crates have a much higher limit.

## Accounts and tokens

Log in with the GitHub account on crates.io, create an API token with the scopes
`publish-new` and `publish-update`, then `cargo login` (it reads the token from standard input
and stores it in `~/.cargo/credentials.toml`). Never write a token into a tracked file, a script
or a commit message.

## Before publishing (on the release commit, a clean checkout)

```sh
cargo fmt --check && cargo clippy --locked --release --workspace --all-targets -- -D warnings
cargo test --locked --release --workspace                  # includes -p hydo-release --test crates
hydo/dist/crates/rehearse.sh                               # the dry run, below
HYDO_REHEARSAL_LINUX=1 hydo/dist/crates/rehearse.sh         # also checks the Linux code (musl target)
```

`hydo/dist/crates/rehearse.sh` is the dry run of the whole publication and of a user's install.
It packs every crate in one `cargo package` call (cargo builds each from its packed `.crate`
against the other packed crates, which is what `cargo publish` checks one at a time), makes a
stand-in for crates.io from the packed crates and the vendored third-party crates, installs
`hydo` from it with `cargo install hydo --locked --offline` in a fresh Cargo home outside the
repository (so no repository file is read and the lock file inside the packed `hydo` decides
every version), and runs the installed `hydo --version`, `hydo guide`, `hydo init` and
`hydo doctor`. Run it from the clean release commit (without `HYDO_REHEARSAL_DIRTY=1`), so that
the packages record that commit.

## The order and the commands

One crate at a time, in this order (every crate after the crates it depends on; the check
above fails when it is not). `cargo publish` builds each crate once more from its package
against what is already on crates.io, uploads it, and then waits until the crates.io index
lists it, so the next command finds it.

```sh
cargo publish --registry crates-io -p hydo-config
cargo publish --registry crates-io -p hydo-schedule
cargo publish --registry crates-io -p hydo-agent-proto
cargo publish --registry crates-io -p hydo-manifest
cargo publish --registry crates-io -p hydo-image
cargo publish --registry crates-io -p hydo-results
cargo publish --registry crates-io -p hydo-history
cargo publish --registry crates-io -p hydo-engine-proto
cargo publish --registry crates-io -p hydo-hvx
cargo publish --registry crates-io -p hydo-hvm
cargo publish --registry crates-io -p hydo-kvm
cargo publish --registry crates-io -p hydo-engine
cargo publish --registry crates-io -p hydo-composer
cargo publish --registry crates-io -p hydo-errors
cargo publish --registry crates-io -p hydo-control
cargo publish --registry crates-io -p hydo-explore
cargo publish --registry crates-io -p hydo-agents
cargo publish --registry crates-io -p hydo-compose
cargo publish --registry crates-io -p hydo-build
cargo publish --registry crates-io -p hydo
```

If a command stops (a network error, the rate limit), run it again: an already published crate
is refused with "already exists", which is safe to skip. `cargo publish --dry-run` of a single
crate works only for a crate whose dependencies are already on crates.io (`hydo-config`,
`hydo-schedule`, `hydo-agent-proto`, `hydo-manifest`, `hydo-image`, `hydo-results` and
`hydo-history` have none); the rehearsal is the dry run of the others.

Publish the crates after the version's GitHub release exists (docs/RELEASING.md): a
`cargo install hydo` of a version whose release archive is not there yet installs a `hydo`
whose `hydo setup --get` cannot download anything.

## Check from outside

On a machine without the repository, in a fresh Cargo home:

```sh
CARGO_HOME=/tmp/hydo-cargo cargo install hydo --locked
/tmp/hydo-cargo/bin/hydo --version            # hydo 0.1.0 (<the release commit>)
/tmp/hydo-cargo/bin/hydo setup --get          # downloads v0.1.0's archive, verified by sha256
/tmp/hydo-cargo/bin/hydo doctor
```

then the quickstart of `hydo/crates/hydo-cli/README.md` (the crates.io page): the Redis
example's clean run and power cut give the fingerprints of docs/REBASELINE.md 13.4 for the
platform.

## The next version

`hydo/VERSION`, `[workspace.package] version` in `Cargo.toml` and the `version` of every
`hydo-*` entry of `[workspace.dependencies]` change together (the check refuses anything else;
`hydo-agent-proto` only when the protocol crate changed, with its own `Cargo.toml` and the guest
agent's re-pin). A workspace version change moves the bytes of `guest/hydo-world` and
`guest/hydo-grpc` (AGENTS.md open item 40), so plan the re-baseline with it. Then the same
commands, in the same order.
