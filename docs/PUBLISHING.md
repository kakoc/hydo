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
