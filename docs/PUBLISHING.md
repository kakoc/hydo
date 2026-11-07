# Publishing the SDKs

hydo's four assertion SDKs are published to their languages' registries with the release's
version, at every release (docs/RELEASING.md step 10). Nothing here is done automatically, and
nothing is published until the user decides to; the packages' metadata and the dry runs below
are prepared in the tree (Milestone 160; the APIs were cleaned up before their first
publication in Milestone 163).

The `hydo` program itself is published through the Homebrew tap (docs/RELEASING.md step 8, `brew install kakoc/tap/hydo`) and the release index (step 9); this file covers the SDKs.

| SDK | registry | name | code-level name | directory |
|---|---|---|---|---|
| Rust | crates.io | `hydo-assert` | `hydo_assert` | `sdk/rust` |
| Python | PyPI | `hydo-assert` | `import hydo_assert` | `sdk/python` |
| JavaScript | npm | `hydo-assert` | `require('hydo-assert')` | `sdk/js` |
| Go | GitHub tag, Go module proxy | `github.com/kakoc/hydo/sdk/go` | packages `hydoassert`, `hydohist` | `sdk/go` |

Every package has the version of `hydo/VERSION` (and of the Cargo workspace), the license
Apache-2.0, a copy of the repository's `LICENSE` and the repository `https://github.com/kakoc/hydo`.
The test `cargo test -p hydo-release --test sdks` checks this, and that the Go module `sdk/go`
holds the same code as the frozen copy in `hydo/stack` apart from the documented differences
(`sdk/go/README.md`, "Two copies").

**Not published: the Rust test SDK.** `hydo-test` (`sdk/rust-test`), `hydo-test-macros`
(`sdk/rust-test/macros`) and `cargo-hydo` (`sdk/rust-test/cargo-hydo`) stay unpublished until the
user decides about code-first tests (user decision, 2026-11-07). Their code stays in the tree
and keeps working (the redis-composed-rs example uses it by path); each has `publish = false`,
which the `sdks` test requires, so `cargo publish` refuses them. Their metadata from Milestone
159 (description, license, repository, `include`, `LICENSE`) is kept for the day they are
published: then remove `publish = false`, add them to this document in dependency order
(`hydo-test-macros` before `hydo-test`, which needs `hydo-assert` on crates.io first) and to
the `sdks` test.

## The API a first publication freezes

A published name cannot be taken back (below), so each SDK's public API is only what a user's
program calls: declaring properties (`always`, `always_or_unreachable`, `sometimes`,
`reachable`, `unreachable`, `eventually`, `buggify`), evaluating them (`check`, `hit`, `cmp`,
`fire`, an eventually property's `begin`/`done`/`expire`/await calls), `catalog`,
`setup_complete`, the details type, and the history recorder with the functions that make its
argument tokens. A comparison has one form, `cmp` with its operator. Everything else is the
SDK's own: in Rust `#[doc(hidden)]` (the `__` items, `Type` and `to_json`, public only because
macros expand in the user's crate or the SDK's tests call them), in Python names starting with
`_`, in JavaScript what `index.d.ts` and `history.d.ts` do not declare, in Go unexported names.
Milestone 163 removed or hid what did not meet this rule (docs/HISTORY.md, "Milestone 163").

## What is permanent

- **crates.io:** a published version can never be deleted or overwritten. It can be *yanked*
  (`cargo yank --version 0.1.0 <crate>`): new projects no longer resolve it, existing lock files
  still download it. A crate name is taken for good once published.
- **PyPI:** a file name can be uploaded only once, ever: deleting a release does not free its
  version, and re-uploading the same version is refused. A release can be *yanked* in the
  project's web page (PEP 592). Deleting a whole project is possible but discouraged and does not
  free its versions.
- **npm:** a version can be unpublished only within 72 hours and only when nothing depends on it;
  after that it can only be *deprecated* (`npm deprecate hydo-assert@0.1.0 "<message>"`). A
  version number, once used, can never be used again, even after unpublishing.
- **Go:** the module proxy caches a tag's content on its first fetch and serves it forever, even
  if the tag is deleted or moved. A bad version is *retracted* with a `retract` directive in the
  `go.mod` of a later version (`retract v0.1.0 // reason`), which makes `go get` skip it.

So check everything before the first upload of a version: the dry runs below, the shared cases
of the four languages, and the version.

## Accounts and tokens

Never write a token into a tracked file, a script or a commit message.

- **crates.io:** log in with the GitHub account on crates.io, create an API token with the
  scope `publish-new` and `publish-update` (Account Settings, API Tokens), then `cargo login`
  (it reads the token from standard input and stores it in `~/.cargo/credentials.toml`).
- **PyPI:** create an account with two-factor authentication and an API token (for the first
  upload a token scoped to the account; afterwards one scoped to the project `hydo-assert`). Put
  it into `~/.pypirc` (then `chmod 600 ~/.pypirc`):

  ```ini
  [pypi]
  username = __token__
  password = <the token>
  ```

  or give it as `TWINE_USERNAME=__token__` and `TWINE_PASSWORD=<the token>` in the environment of
  the upload command.
- **npm:** `npm login --registry https://registry.npmjs.org/` (an account with two-factor
  authentication; `npm publish` then asks for the one-time code).
- **Go:** nothing: the tag is pushed to the GitHub repository.

A developer's npm configuration can name private registries (for other scopes, or as the
default), so every npm command below names the public registry explicitly. `sdk/js/package.json`
gets no `publishConfig` (its bytes are inside the node-kv images). `crates-io` is cargo's default
registry and PyPI twine's default repository; the commands name them anyway.

## Before publishing (on the release commit)

```sh
cargo test --locked --release -p hydo-release --test sdks              # names, versions, licenses, Go copies
(cd sdk/rust && cargo test)                                            # the Rust SDK and its shared cases
(cd sdk/rust-test && cargo test --workspace)                           # unpublished, but it uses hydo-assert
(cd sdk/go && go vet ./... && go test ./...)                            # and GOTOOLCHAIN=go1.26.5 go test ./...
(cd hydo/stack && go test ./hydoassert/ ./hydohist/)                    # the frozen copy
python3 -m unittest discover -s sdk/python/tests
(cd sdk/js && npm test && node --test test/cases.test.js)
```

## The order and the commands

The four registries are independent; do them in the order below so that each SDK's README links
work when users arrive. Each upload is preceded by its dry run.

1. **`hydo-assert` (crates.io)**

   ```sh
   cd sdk/rust
   cargo publish --dry-run --registry crates-io
   cargo publish --registry crates-io
   ```

2. **`hydo-assert` (PyPI)**: build in a throwaway virtual environment, from a copy of `sdk/python`
   (setuptools writes `src/hydo_assert.egg-info` next to the sources).

   ```sh
   python3 -m venv /tmp/hydo-pypi && /tmp/hydo-pypi/bin/pip install build twine
   rm -rf /tmp/hydo-py && cp -R sdk/python /tmp/hydo-py && find /tmp/hydo-py -name __pycache__ -prune -exec rm -rf {} +
   /tmp/hydo-pypi/bin/python -m build --outdir /tmp/hydo-py-dist /tmp/hydo-py
   /tmp/hydo-pypi/bin/twine check --strict /tmp/hydo-py-dist/*
   /tmp/hydo-pypi/bin/twine upload --repository pypi /tmp/hydo-py-dist/hydo_assert-0.1.0*
   ```

   (Optionally first `--repository testpypi` with a TestPyPI token, as a rehearsal: TestPyPI has
   the same rules and is a separate account.)

3. **`hydo-assert` (npm)**

   ```sh
   cd sdk/js
   npm publish --dry-run --registry https://registry.npmjs.org/
   npm publish --registry https://registry.npmjs.org/
   ```

4. **The Go module**: a tag on the release commit, pushed to GitHub, then one fetch through the
   proxy so that the version is listed at once.

   ```sh
   git tag -a sdk/go/v0.1.0 -m "hydo Go SDK 0.1.0"
   git push origin sdk/go/v0.1.0
   GOPROXY=https://proxy.golang.org GOFLAGS=-mod=mod go list -m github.com/kakoc/hydo/sdk/go@v0.1.0
   ```

   The tag's prefix `sdk/go/` is the module's directory in the repository; Go finds a module in a
   subdirectory only by such a tag. pkg.go.dev shows the documentation after the first fetch.

5. **Check from outside:** in an empty directory, `cargo add hydo-assert`,
   `pip install hydo-assert` in a new virtual environment,
   `npm install hydo-assert --registry https://registry.npmjs.org/`, and a Go module with
   `go get github.com/kakoc/hydo/sdk/go@v0.1.0`, each importing the package.

## The next version

A release changes the version of every SDK at once (the `sdks` test refuses anything else):
`hydo/VERSION`, the workspace version in `Cargo.toml`, `sdk/rust/Cargo.toml` (and, to keep the
unpublished test SDK in step, the three `sdk/rust-test` `Cargo.toml` files, the two dependency
versions in `sdk/rust-test/Cargo.toml` and `sdk/rust-test/Cargo.lock`),
`sdk/python/pyproject.toml` and `__version__` in `sdk/python/src/hydo_assert/__init__.py`,
`sdk/js/package.json`, and the Go tag. A version change moves bytes inside recorded images, so
plan the re-baseline with it (docs/REBASELINE.md):

- the Rust SDK's version is part of Cargo's symbol hashes, so `rkv` (rust-kv) and the
  redis-composed-rs guest binary get new bytes;
- `sdk/js/package.json` is copied into the node-kv images;
- `sdk/python/src/hydo_assert/__init__.py` is copied into the python-kv and python-kv-hist images;
- the workspace version changes `guest/hydo-world` and `guest/hydo-grpc` (AGENTS.md open item 40).

The Go SDK's tag changes no image (the order service uses the frozen copy in `hydo/stack`).

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
  `hydo/dist/<platform>/assets.sha256` and `guest-programs.sha256`, `hydo-image`'s `releases.rs` the
  release index `hydo/dist/releases.txt` (Milestone 162), and `hydo-agents`' build script embedded `skills/hydo-onboard/` and `hydo/onboard/MANIFEST.md`. Each crate now reads
  them through links inside its directory (`hydo-cli/pins/<platform>/*.sha256`, `hydo-image/pins/releases.txt`,
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
  around it, and took `~/.cargo` as its release root. Now, when the program's prefix is not an
  unpacked release (no `SHA256SUMS`) and cargo's install record `.crates2.json` there names the
  package `hydo`, the release root is `$HYDO_HOME/versions/<its version>` (`hydo-cli/src/ctx.rs`,
  `installed_root`); plain `hydo setup` then does what `hydo setup --get` does, and
  `hydo doctor` names `hydo setup --get` as the fix for every missing release file. Every other
  layout (a release, the CLI tests' release-like roots, a tree's `target/`) is unchanged.
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
