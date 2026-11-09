# hydo's assertion SDKs

A program that runs under [hydo](https://github.com/kakoc/hydo) uses an assertion SDK to say
three things from the inside:

- what must be true and what should happen (properties);
- where it may misbehave on purpose (fault points, also called BUGGIFY);
- which operations its clients performed (operation histories, which hydo checks after the run).
 The four SDKs have the same calls, in
each language's style, and write the same console records, byte for byte.

| language | package | its page |
|---|---|---|
| Rust | `hydo-assert` on crates.io | [sdk/rust](https://github.com/kakoc/hydo/tree/main/sdk/rust) |
| Python | `hydo-assert` on PyPI | [sdk/python](https://github.com/kakoc/hydo/tree/main/sdk/python) |
| JavaScript (Node.js) | `hydo-assert` on npm | [sdk/js](https://github.com/kakoc/hydo/tree/main/sdk/js) |
| Go | the module `github.com/kakoc/hydo/sdk/go` | [sdk/go](https://github.com/kakoc/hydo/tree/main/sdk/go) |

Each page has the same parts:

1. install;
2. one complete program with the lines it writes;
3. how to declare and check properties, compare values, and check that something happens in
   time;
4. how to add a fault point, record an operation history, and choose where the records go;
5. the reference.

The records and how hydo judges them are in
[hydo/ASSERTIONS.md](https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md); the histories
and their checkers in [hydo/HISTORIES.md](https://github.com/kakoc/hydo/blob/main/hydo/HISTORIES.md).

`sdk/rust-test` is a different thing: hydo tests written as one Rust program (`hydo-test`),
not published.

The rest of this page is for people who change the SDKs.

## The pages are tested

Every code example on the four pages runs in the SDK's own tests. The complete program's output
must be the lines its page shows, the same lines on the four pages.
`cargo test -p hydo-release --test it sdks::` compares those lines and the pages' headings.

- **Rust:** `README.md` is the crate documentation (`#![doc = include_str!("../README.md")]`),
  so every `rust` block is a doctest (`cargo test --doc` in `sdk/rust`). `tests/readme.rs` checks
  that the complete program is `examples/kv_store.rs` and runs it.
- **Python:** `tests/test_readme.py` runs every `python` block as its own program.
- **JavaScript:** `test/readme.test.js` runs every `js` block as its own program (`node --test`;
  `npm test`, whose script is inside recorded images, runs only the unit tests).
- **Go:** the complete program runs with `go run` in a new module, and every other `go` block
  must be a piece of an example function of `hydoassert/example_test.go` or
  `hydohist/example_test.go`, which `go test` runs and checks (`TestReadme`).

## SDK files inside recorded guest images

Some example images contain SDK files, so changing those files changes the images' bytes and
every recorded fingerprint of those examples (docs/REBASELINE.md has the values):

- **Rust:** the version in `sdk/rust/Cargo.toml` is part of the symbol names of `rkv` (rust-kv)
  and of the redis-composed-rs program. The line numbers of `src/assertion.rs` and
  `src/buggify.rs` are inside `rkv` too (the locations of its panic messages), so a change of
  those two files must keep every line of code on its line. Documentation elsewhere (the
  README, which is the crate documentation, and doc comments in the other files) changes no
  byte of either program.
- **Python:** every `.py` file of `sdk/python/src/hydo_assert/` is copied into the python-kv and
  python-kv-hist images, docstrings included.
- **JavaScript:** `package.json`, `index.js` and `index.mjs` are copied into the node-kv images
  (`index.d.ts`, `history.d.ts`, the README and the tests are not).
- **Go:** nothing in `sdk/go`; the order service is built from the frozen copy in `hydo/stack`
  (below).

## Testing each SDK

```sh
(cd sdk/rust && cargo test --features steer)   # the doctests, the shared cases, the README program
python3 -m unittest discover -s sdk/python/tests
(cd sdk/js && node --test)                     # every file in test/
(cd sdk/go && go vet ./... && go test ./...)   # and GOTOOLCHAIN=go1.26.5 go test ./...
(cd hydo/stack && go test ./hydoassert/ ./hydohist/)
cargo test -p hydo-release --test it sdks::    # names, versions, licenses, the Go copies, the pages
```

## The shared cases

`sdk/cases/` holds example files. Each one says which bytes a call writes under a given
configuration. The Rust SDK's test `sdk/rust/tests/cases/` writes them. The Go, Python, and
JavaScript SDKs each have a test that reads every case, makes the same calls with its own API,
and compares the bytes:

- `go test ./...` in `sdk/go` and in its frozen copy `hydo/stack/hydoassert`;
- `python3 -m unittest discover -s sdk/python/tests`;
- `node --test` in `sdk/js`.

No SDK test needs another language installed.

```sh
cargo test --features steer --test cases                # in sdk/rust: sdk/cases must equal what the Rust SDK writes
HYDO_BLESS=1 cargo test --features steer --test cases   # in sdk/rust: rewrite sdk/cases (and remove files no case writes)
```

**A format change starts in the Rust SDK:** change it, run with `HYDO_BLESS=1`, and commit the
changed files in `sdk/cases`. The other SDKs' case tests then fail on the lines they must change.
**To add a case:** add a program to `sdk/rust/tests/cases/defs.rs` (the builder `Prog` checks the calls),
bless, commit. `sdk/rust/tests/cases/programs.rs` holds the call sequences of the Go-binary conformance
programs these cases replaced (M110); their outputs equal Go 1.26.5's.

**Files.** `sdk/cases/<area>/<name>.json` is one case; each of its runs has an output file,
`<name>.out` (a case with one unnamed run) or `<name>.<run>.out`. Areas: `assert` (the types and
deduplication), `compare`, `details` (the JSON encoding), `catalog`, `eventually`, `buggify`,
`output` (`HYDO_ASSERT_OUT`), `history` (`HYDO-HIST` records and
`HYDO_HIST_OUT`), `programs`, `steer` (steered values, `HYDO-STEER` records; only the Rust SDK has them, and the other readers skip these cases by their need `steer`). The case `history/messages` holds messaging records written with
generic steps, whose item tokens come from the Rust SDK's `history::messages::Item`. The other
SDKs replay it with their generic recorders. `sdk/rust/tests/messages.rs` checks that the Rust
`Messages` recorder writes the same lines. A case file has:

- `about`: what the case shows.
- `needs`: features a language may lack. They are `f32` (a float32 value), `bytes` (a byte
  string, Go's `[]byte`), `invalid_utf8` (invalid UTF-8 in an assertion string), `raw_hist`
  (invalid UTF-8 in a history record) and `steer` (the steered-value calls). A reader skips, and
  reports, a case that needs what its language cannot express, and fails on a need it does not
  know.
- `steps`: the calls, in order. The declarations are `always`, `always_or_unreachable`,
  `sometimes`, `reachable`, `unreachable` (`id`, `msg`), `eventually` (`within_ms`), `buggify`, and `minimize` and `maximize` (a steered value; `value` with `v` reports a number).
  They are numbered from 0 in the order they appear and named by `a`. Then come `check` (`cond`), `hit`,
  `cmp` (`left`, `op`, `right`), `begin` (its watches are numbered and named by `w`), `done`, `expire`, `await`
  (the condition is true at once; `async`: the async form), `fire`, `catalog`,
  `setup_complete`. Any of them may carry `details` (a map
  value). History steps: `history` (`name`; numbered, named by `h`), `invoke` (`proc`, `f`,
  `args`; numbered, named by `o`), `ok` (`args`; empty only for an operation without
  arguments), `ok_same` (the invocation's arguments, as `ok()` takes them in Go, JavaScript, and Python),
  `fail`, `info`, `record` (`proc`, `type`, `f`, `args`).
- Values carry their Go type. A value is one of these:
  - `null`, `true`/`false`, a JSON string, or a list;
  - `{"int": "<decimal>"}` (int64) or `{"uint": ...}` (uint64);
  - `{"f64": "<number>"}` or `{"f32": ...}`, with `NaN`, `+Inf`, and `-Inf`;
  - `{"bytes": "<hex>"}` or `{"map": [[key, value], ...]}`;
  - `{"raw": ["text", 255, ...]}`, a string with invalid UTF-8 bytes, each number one byte.
    Python passes it as `chr(0xDC00 + byte)`, and JavaScript as that lone surrogate.
- A history argument is a token (a string or `raw`) or one of these helpers:
  - `{"enc": s}`, `{"ver": [s, "<version>"]}`, `{"value_or_nil": s or null}`;
  - `{"list": [...]}`, `{"append_mop": [k, e]}`, `{"read_mop": k}`;
  - `{"read_result_mop": [k, [...]]}`, and `{"nil": true}` (the constant).
- `runs`: each run has these fields:
  - `out`: where assertion records go. It is `file` (an empty file), `append` (a file holding the
    line `PRE existing line`), `stderr`, `missing` (a path that does not exist), or `off`.
  - `hist_out`: the same for history records, with `create` (a missing file) and `fallback` (a
    path in a missing directory).
  - `buggify_env`: the value of `HYDO_BUGGIFY`; null means unset.
  - `buggify_file`: the content of the file that `HYDO_BUGGIFY_FILE` names; null means that it
    names a missing file.
  - `output`: the file with the expected bytes.
  - `returns`: the results of `check`, `cmp`, `done`, `await`, and `fire`, as `1` or `0`, in
    order.

**Reading a run.** Start a fresh process, because SDK state is per process. Set the four
variables and make the calls. Then take what the one sink that is not `off` received: the file,
or standard error for `stderr`, `missing`, and `fallback`. It must equal the output file byte for
byte, and standard error must stay empty otherwise. In a `HYDO-HIST` record the time field (the
fifth) is `*` in the output file. Replace it before comparing, and check that the times are
decimal integers that never decrease. The Rust recorder writes `-` where `/proc/uptime` is
missing.

**Stand-ins in the Rust runner.** Rust has neither `[]byte` nor strings with invalid UTF-8. It
passes a byte string as its base64 text, which is what Go writes for `[]byte`. It passes each
invalid byte as the character U+10FF00 + byte, which the test turns into what Go 1.26 writes (`\ufffd` inside
JSON, the raw byte in a history record). Keys with invalid bytes are chosen so that every SDK
sorts them the same (the builder checks it). The history `ver` takes a `u64`, so the cases
have no negative versions (the other SDKs' unit tests cover those).

## The Go module and the frozen copy in `hydo/stack`

The same code lives in two places:

- **`sdk/go` (the module `github.com/kakoc/hydo/sdk/go`)**: the public module that users
  import.
- **`hydo/stack/hydoassert` and `hydo/stack/hydohist`** (inside the module `hydostack`): a
  frozen copy. The order service of the stack example and the bug benchmark imports it, and
  that service's compiled bytes are inside recorded guest images. Changing that module's path or
  those files would change the order service's bytes and move recorded fingerprints, so the
  copy there stays as it is.

The test `cargo test -p hydo-release --test it sdks::` (in the repository's Cargo workspace) fails
when the two copies differ in anything but these documented differences. Exactly these may
differ, and nothing else. A file on one side only also fails. The exceptions are the shell SDK
and the examples:

1. Three lines of `hydoassert/cases_test.go`, each exactly once in each copy:

   | `hydo/stack` | `sdk/go` | why |
   |---|---|---|
   | `"hydostack/hydoassert"`, `"hydostack/hydohist"` | `"github.com/kakoc/hydo/sdk/go/hydoassert"`, `.../hydohist` | the module path |
   | `const casesDir = "../../../sdk/cases"` | `const casesDir = "../../cases"` | where the shared cases are from the package directory |
   | `(in hydo/stack/hydoassert)` | `(in sdk/go)` | the comment that says where to run the tests |

2. Seven identifiers in every file of `hydoassert` (Milestone 163). The public module keeps
   unexported what no call of its API takes or returns. So the assertion type and its constants
   are lower case in `sdk/go` and exported in the frozen copy:

   | `hydo/stack` | `sdk/go` |
   |---|---|
   | `Type` | `kind` |
   | `TAlways`, `TAlwaysOrUnreachable`, `TSometimes`, `TReachable`, `TUnreachable`, `TEventually` | `tAlways`, `tAlwaysOrUnreachable`, `tSometimes`, `tReachable`, `tUnreachable`, `tEventually` |

   The test maps every whole-word occurrence of the `sdk/go` names back to the frozen names. Then
   it compares the files byte for byte, so the copies can differ only by the rename, also in
   comments. The test also fails in two more cases. A frozen name is still a word in `sdk/go`:
   the rename must be complete, so the public module never exports them. An `sdk/go` name is a
   word in `hydo/stack`: the mapping must stay one to one.

The public API is therefore: `Always`, `AlwaysOrUnreachable`, `Sometimes`, `Reachable`,
`Unreachable` (an `*Assertion` with `Check`, `Hit`, `Cmp`), `Eventually` (a `*Liveness` with
`Await` and `Begin`, a `*Watch` with `Done` and `Expire`), `Buggify` (a `*Point` with `Fire`),
`Catalog`, `SetupComplete` and the details type `D`; and in `hydohist` `New`, `History`
(`Invoke`, `Record`), `Op` (`Ok`, `Fail`, `Info`), `Nil` and the token functions `Enc`, `Ver`,
`ValOrNil`, `List`, `Append`, `Read`, `ReadResult`. A comparison has one form, `Cmp` with its
operator, as in the other SDKs.

`hydo/stack/hydoassert/hydo_assert.sh` (the shell SDK) is not part of the public module.
`sdk/go/hydoassert/example_test.go` and `sdk/go/hydohist/example_test.go` (the package examples
and the README's test) are not part of the frozen copy, because test files are compiled into no
program. Make a change to the Go SDK in both places in one commit. A change to the frozen copy
changes the order service's bytes, so it needs a re-baseline of the images that contain it
(docs/REBASELINE.md).

## Publishing

docs/PUBLISHING.md has the steps, in order, and what a published version makes permanent.
