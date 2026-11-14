# Publishing from GitHub Actions (trusted publishing)

Since Milestone 186 the registry packages of a release are published by the workflow
`.github/workflows/release.yml`, with each registry's trusted publishing (user decision,
2026-11-08): GitHub gives the workflow a short-lived OpenID Connect token that says which
repository, workflow and environment it runs in, and the registry, which was told once to
trust exactly that combination, accepts it for one upload. No registry token is stored in the
repository, in GitHub's secrets or on a laptop, and nobody's second factor is needed per upload
(npm asks for one at every publish from a laptop, which blocked unattended releases). The
commands in the sections after this one stay as the fallback (publishing by hand).

## What the workflow does

It starts when a version tag `v<version>` appears in the repository: creating the GitHub release
with `gh release create v<version> --target <commit> …` creates the tag (docs/RELEASING.md step 7),
and a tag pushed by hand starts it too. It can also be started by hand (Actions, `release`, Run
workflow, or `gh workflow run release.yml --ref <tag or branch> -f dry_run=<true|false>`); the
input `dry_run` (on by default) runs everything except the uploads, also on a branch, where the
version is `hydo/VERSION`'s. A run by hand without `dry_run` must be on the tag.

| Job | What it does | Permissions |
|---|---|---|
| `check` | the tag's version equals `hydo/VERSION`, the workspace's, every `hydo-*` entry of `[workspace.dependencies]` (not `hydo-agent-proto`, which keeps its own) and every SDK's (`hydo/dist/registries.sh versions`); the GitHub release of the tag is published with the four tarballs and `SHA256SUMS`, and `SHA256SUMS` names every tarball (`registries.sh release`; for a pushed tag it waits up to 45 minutes; a dry run only warns); it prints what each registry already has and what the run would publish | `contents: read` |
| `packages` | `.github/workflows/packages.yml` (below): every package packed and checked, and `cargo install hydo` rehearsed, on Linux and macOS; it builds the Python sdist and wheel that the `pypi` job uploads | `contents: read` |
| `crates` | the 20 crates of `cargo install hydo` in this document's order (the `cargo publish --registry crates-io -p` lines below), then `hydo-assert` (`sdk/rust`), with `cargo publish --locked --no-verify` (the `packages` job built every package); a token from `rust-lang/crates-io-auth-action` (a trusted-publishing token lives 30 minutes: the job stops starting crates after 20 minutes and takes a second token for the rest) | `contents: read`, `id-token: write`; environment `release` |
| `pypi` | the sdist and wheel from the `packages` job, with `pypa/gh-action-pypi-publish` (trusted publishing, attestations on) | `id-token: write`; environment `release` |
| `npm` | `npm publish --provenance --access public --registry https://registry.npmjs.org/` in `sdk/js`, with npm 11.21.0 on Node 22 (npm's trusted publishing needs npm 11.5.1 or later and Node 22.14.0 or later) | `contents: read`, `id-token: write`; environment `release` |
| `go` | the annotated tag `sdk/go/v<version>` on the tagged commit (through the GitHub API, with the workflow's own token), then one fetch of the version through `proxy.golang.org` | `contents: write`; environment `release` |

Every job has a timeout, every action is pinned by commit, no job restores a cache, and only
the four publishing jobs run in the GitHub environment `release`, which every registry's
trusted-publisher entry names: a token from any other workflow, job or fork is refused by the
registries. A dry run skips those four jobs. Every publishing job skips a version its registry
already has (crates.io's index, `skip-existing` on PyPI, the npm registry, an existing Go tag on
the same commit; an existing Go tag on another commit fails the job), so **a failed job is
re-run as it is** (Actions, Re-run failed jobs) once the cause is fixed; a version that is
already uploaded is never uploaded again.

`.github/workflows/packages.yml` is the dry run of every package on its own: it runs on every
pull request and push to main that changes a package (the Cargo workspace, the crates, the
files they compile in, the SDKs, this document) and by hand. Its jobs: `crates` (Linux and
macOS: `hydo/dist/crates/rehearse.sh`, then `cargo package` of `sdk/rust`), `python` (the sdist
and the wheel built as below, `twine check --strict`), `npm` (`npm pack --dry-run`, and a check
of the files it would publish). It uploads nothing.

`hydo/dist/registries.sh` holds what the jobs run (the order, the version check, the release
check, the published-version lookups and the publishing loop); each command works by hand too.

## The one-time setup on each registry

Every registry needs the same four values, taken from the repository `kakoc/hydo`: its owner
(the part before the slash), its name `hydo`, the workflow file name `release.yml` (the name
only, not the path) and the environment `release`.
The packages exist on every registry since 0.2.0 (crates.io and npm let a trusted publisher be
added only to an existing package).

1. **GitHub, the environment.** Settings, Environments, New environment `release` (the workflow
   would create it on first use, without rules). Optional: Deployment branches and tags,
   "Selected branches and tags", a tag rule `v*` (then only a run on a version tag can enter the
   environment); Required reviewers, the user (then every release waits for one approval on the
   run's page before anything is uploaded). No environment secret is needed.
2. **crates.io**, for each of the 21 crates that `hydo/dist/registries.sh crates` prints (the 20
   of the `cargo publish --registry crates-io -p` lines below, then `hydo-assert`): the crate's
   page, Settings, Trusted Publishing, Add: publisher GitHub, repository owner and repository
   name those of `kakoc/hydo`, workflow filename `release.yml`, environment `release`. A crate with no entry fails in
   the `crates` job with an authorization error; add the entry and re-run the job. A crate that
   is new to the closure must be published once by hand with a token (crates.io accepts a
   trusted publisher only for a crate that exists), then gets its entry.
3. **PyPI**, the project `hydo-assert`: Manage, Publishing, Add a new publisher, GitHub: owner
   and repository name those of `kakoc/hydo`, workflow name `release.yml`, environment name
   `release`.
4. **npm**, the package `hydo-assert`: npmjs.com, the package, Settings, Trusted Publisher,
   GitHub Actions: organization or user and repository those of `kakoc/hydo`, workflow filename
   `release.yml`, environment name `release`. npm's documentation says a new trusted-publisher
   configuration must complete its first successful publish within 2 days, or it expires and can
   no longer be used or edited: add it at most two days before the release (and add it again if
   it expired). After the first publish through it, npm recommends Settings, Publishing access,
   "Require two-factor authentication and disallow tokens": the workflow still publishes, and no
   token can.
5. **Go:** nothing on a registry. The `go` job pushes the tag with the workflow's own token; if a
   ruleset of the repository restricts creating tags, allow the GitHub Actions app to create
   `sdk/go/v*` tags (or push the tag by hand, as below, before the workflow's run reaches it:
   the job accepts a tag already on the same commit).

Before the first real release with the workflow, a dry run checks everything except the
uploads and the trusted-publisher entries: `gh workflow run release.yml --ref main -f dry_run=true`.

# Publishing the SDKs

hydo's four assertion SDKs are published to their languages' registries with the release's
version, at every release (docs/RELEASING.md step 10), by the release workflow (above); the
commands below are the same steps by hand, the fallback. The packages' metadata and the dry
runs below are prepared in the tree (Milestone 160; the APIs were cleaned up before their first
publication in Milestone 163).

The `hydo` program itself is published through the GitHub release (docs/RELEASING.md step 7: the two tarballs, the two sources archives and, since 0.2.0, the Mac programs' signatures asset `hydo-mac-<version>-signatures.tar.gz`, docs/design/signing.md), the Homebrew tap (step 8, `brew install kakoc/tap/hydo`) and the release index (step 9); this file covers the SDKs, the crates of `cargo install hydo`, and (last section) the onboarding skill, which has its own releases.

| SDK | registry | name | code-level name | directory |
|---|---|---|---|---|
| Rust | crates.io | `hydo-assert` | `hydo_assert` | `sdk/rust` |
| Python | PyPI | `hydo-assert` | `import hydo_assert` | `sdk/python` |
| JavaScript | npm | `hydo-assert` | `require('hydo-assert')` | `sdk/js` |
| Go | GitHub tag, Go module proxy | `github.com/kakoc/hydo/sdk/go` | packages `hydoassert`, `hydohist` | `sdk/go` |

Every package has the version of `hydo/VERSION` (and of the Cargo workspace), the license
Apache-2.0, a copy of the repository's `LICENSE` and the repository `https://github.com/kakoc/hydo`.
The test `cargo test -p hydo-release --test it sdks::` checks this, and that the Go module `sdk/go`
holds the same code as the frozen copy in `hydo/stack` apart from the documented differences
(`sdk/README.md`, "The Go module and the frozen copy in `hydo/stack`").

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

## The pages

Each SDK's `README.md` is its page on the registry (and on pkg.go.dev for Go). The four pages
have the same parts in the same order, and every code example on them runs in the SDK's own
tests (`sdk/README.md`, "The pages are tested"); `cargo test -p hydo-release --test it sdks::`
checks that the parts and the complete programs' lines are the same on the four pages. The
links on the pages are absolute, because the registries show a README outside the repository.

## What is permanent

- **crates.io:** a published version can never be deleted or overwritten. It can be *yanked*
  (`cargo yank --version 0.3.0 <crate>`): new projects no longer resolve it, existing lock files
  still download it. A crate name is taken for good once published.
- **PyPI:** a file name can be uploaded only once, ever: deleting a release does not free its
  version, and re-uploading the same version is refused. A release can be *yanked* in the
  project's web page (PEP 592). Deleting a whole project is possible but discouraged and does not
  free its versions.
- **npm:** a version can be unpublished only within 72 hours and only when nothing depends on it;
  after that it can only be *deprecated* (`npm deprecate hydo-assert@0.3.0 "<message>"`). A
  version number, once used, can never be used again, even after unpublishing.
- **Go:** the module proxy caches a tag's content on its first fetch and serves it forever, even
  if the tag is deleted or moved. A bad version is *retracted* with a `retract` directive in the
  `go.mod` of a later version (`retract v0.3.0 // reason`), which makes `go get` skip it.

So check everything before the first upload of a version: the dry runs below, the shared cases
of the four languages, and the version.

## Accounts and tokens (publishing by hand)

The release workflow needs none of these (above). Never write a token into a tracked file, a
script or a commit message.

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
cargo test --locked --release -p hydo-release --test it sdks::              # names, versions, licenses, Go copies
(cd sdk/rust && cargo test)                                            # the Rust SDK and its shared cases
(cd sdk/rust-test && cargo test --workspace)                           # unpublished, but it uses hydo-assert
(cd sdk/go && go vet ./... && go test ./...)                            # and GOTOOLCHAIN=go1.26.5 go test ./...
(cd hydo/stack && go test ./hydoassert/ ./hydohist/)                    # the frozen copy
python3 -m unittest discover -s sdk/python/tests
(cd sdk/js && node --test)                                             # every test, the README's too
```

## The order and the commands (by hand)

The release workflow runs these steps (its `crates`, `pypi`, `npm` and `go` jobs, in parallel).
By hand: the four registries are independent; do them in the order below so that each SDK's
README links work when users arrive. Each upload is preceded by its dry run.

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
   /tmp/hydo-pypi/bin/twine upload --repository pypi /tmp/hydo-py-dist/hydo_assert-0.3.2*
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
   git tag -a sdk/go/v0.3.2 -m "hydo Go SDK 0.3.2"
   git push origin sdk/go/v0.3.2
   GOPROXY=https://proxy.golang.org GOFLAGS=-mod=mod go list -m github.com/kakoc/hydo/sdk/go@v0.3.2
   ```

   The tag's prefix `sdk/go/` is the module's directory in the repository; Go finds a module in a
   subdirectory only by such a tag. pkg.go.dev shows the documentation after the first fetch.

5. **Check from outside:** in an empty directory, `cargo add hydo-assert`,
   `pip install hydo-assert` in a new virtual environment,
   `npm install hydo-assert --registry https://registry.npmjs.org/`, and a Go module with
   `go get github.com/kakoc/hydo/sdk/go@v0.3.2`, each importing the package.

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
(Milestone 169). At a release the `crates` job of the release workflow publishes them (above,
"Publishing from GitHub Actions"); the commands below are the same steps by hand, the
fallback. The crates' metadata, the check and the dry run below are prepared in the tree. The
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

## The crates' pages

crates.io shows each crate's `README.md`, and docs.rs shows its documentation. For every crate
of the closure the two are one text: the first line of a library's `src/lib.rs` is
`#![doc = include_str!("../README.md")]`, so a code block of the page is a doctest. Every link on
a page is absolute (crates.io shows the README outside the repository). The `hydo` crate's page
says what hydo is, how to install it and how to make a first run; its test
(`cargo test -p hydo --test it readme::`) checks every `hydo` command the page shows against
the help text, and runs those that need no installed release. The page of each of the other 19
crates is short and the same in form: that the crate is part of hydo, that it has no stable API
("This crate has no stable API."), how to get hydo instead, and what the crate contains. After
the page come the crate's notes for hydo's developers. The `crates` check below fails when a
page is missing, a link is relative, a library's documentation does not start with its page, or
an internal crate's page lacks that sentence; `cargo doc --no-deps` of the 20 crates builds
without a warning.

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
  release index `hydo/dist/releases.txt` (Milestone 162), and `hydo-agents`' build script embedded the skill folder (since Milestone 229 the plugin folder `plugin/`) and `hydo/onboard/MANIFEST.md`. Each crate now reads
  them through links inside its directory (`hydo-cli/pins/<platform>/*.sha256`, `hydo-image/pins/releases.txt`,
  `hydo-agents/embed/plugin` and `embed/MANIFEST.md`); git keeps the links, the tree
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

The check `cargo test -p hydo-release --test it crates::` fails when any crate the `hydo` package
depends on is not publishable: missing metadata, a version other than `hydo/VERSION`, a
dependency without its version, a dev-dependency with a version, tests in the package, a file
it includes from outside its directory, a build script that names a path outside the crate (one
documented exception, `hydo-image`'s tree-only `../../COMMIT`), more than 10 MiB of files, a
guest program in the closure, a workspace crate outside the closure without `publish = false`,
or an order below that publishes a crate before its dependencies (`hydo/crates/hydo-release/src/publish.rs`).

## What is permanent

A published version can never be deleted or overwritten; it can only be *yanked*
(`cargo yank --version 0.3.0 <crate>`): new projects no longer resolve it, existing lock files
still download it. A crate name is taken for good once published. So run the dry run below on
the release commit before the first upload.

crates.io limits how fast new crates can be created (a burst of a few, then one about every ten
minutes, at the time of writing): publishing 20 new crates takes a few hours unless crates.io is
asked to raise the limit for the account first (help@crates.io). New versions of existing
crates have a much higher limit.

## Accounts and tokens (publishing by hand)

Only for publishing by hand, and for the first version of a crate that is new on crates.io
(crates.io accepts a trusted publisher only for an existing crate). Log in with the GitHub
account on crates.io, create an API token with the scopes
`publish-new` and `publish-update`, then `cargo login` (it reads the token from standard input
and stores it in `~/.cargo/credentials.toml`). Never write a token into a tracked file, a script
or a commit message.

## Before publishing (on the release commit, a clean checkout)

```sh
cargo fmt --check && cargo clippy --locked --release --workspace --all-targets -- -D warnings
cargo test --locked --release --workspace                  # includes -p hydo-release --test it crates::
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

This list is the order: the release workflow's `crates` job, `hydo/dist/registries.sh crates`
and `hydo/dist/crates/rehearse.sh` read these lines, and the `crates` test checks them. By
hand: one crate at a time, in this order (every crate after the crates it depends on; the check
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
whose `hydo setup --get` cannot download anything. The release workflow's `check` job refuses
to go on until the release is published with its assets.

## Check from outside

On a machine without the repository, in a fresh Cargo home:

```sh
CARGO_HOME=/tmp/hydo-cargo cargo install hydo --locked
/tmp/hydo-cargo/bin/hydo --version            # hydo 0.3.2 (<the release commit>)
/tmp/hydo-cargo/bin/hydo setup --get          # downloads v0.3.2's archive, verified by sha256
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

## The onboarding skill (Milestone 170)

The skill has its own channels and its own version (docs/RELEASING.md, "Releasing the onboarding
skill"); publishing it is the user's decision, like everything else here.

| Channel | What | Updated by |
|---|---|---|
| GitHub release `skill-v<version>` | `hydo-skill-<version>.tar.gz` and `SHA256SUMS`, read by `hydo skill update` | docs/RELEASING.md, "Releasing the onboarding skill", step 5 |
| Claude Code marketplace `.claude-plugin/marketplace.json` on `main` (since 0.2.0) (`/plugin marketplace add kakoc/hydo`, `/plugin install hydo@hydo`) | the folder `plugin/` as the plugin `hydo`, whose skill `onboard` is the command `/hydo:onboard`, at the version its entry names | the same section, step 3 |

The skill is published separately from hydo (Milestone 170), so its process can improve between
hydo releases while the facts always come from the user's own hydo (`hydo guide`):

- One plugin folder, `plugin/`, in Claude Code's standard plugin layout
  (`.claude-plugin/plugin.json` and `skills/onboard/`, Milestone 229). The skill folder
  `plugin/skills/onboard/` is in the Agent Skills format (https://agentskills.io/specification),
  the same files for every tool. Claude Code takes the plugin from the marketplace entry, whose
  `source` is `./plugin`, so nothing is copied or linked; `hydo skill install --agent claude`
  copies the plugin folder to `.claude/skills/hydo`, which Claude Code loads as the same plugin.
  Codex, Cursor, GitHub Copilot and Gemini CLI read the skill folder that `hydo skill install
  --agent <tool>` writes into `.agents/skills/onboard`.
- A user of another tool gets a new skill version with `hydo skill update`, which runs only when
  the user runs it, takes the newest `skill-v<version>` release from the GitHub API, and accepts
  the archive only with the sha256 that the release's `SHA256SUMS` (fetched over TLS) names. The
  skill itself never downloads anything.
- A published skill release is never changed: a fix is a new version. A skill version states the
  oldest hydo it supports (`metadata.hydo-min`); a skill that needs a newer guide topic raises it.
