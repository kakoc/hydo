# hydo's Go SDK: `hydoassert` and `hydohist`

A Go program running under hydo (the deterministic hypervisor in this repository) uses these
packages to say what must be true and what should happen at least once (`hydoassert`), and to
record the operations a workload client performs so that the host can check the history after
the run (`hydohist`). The wire format and the verdict rules are in
[`hydo/ASSERTIONS.md`](https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md) and
[`hydo/HISTORIES.md`](https://github.com/kakoc/hydo/blob/main/hydo/HISTORIES.md). The packages
use only the standard library and need Go 1.22 or later (Go 1.21's `encoding/json` writes
`\b` and `\f` as `\u0008` and `\u000c`, which are not the SDK's bytes).

```sh
go get github.com/kakoc/hydo/sdk/go@v0.1.0
```

```go
import (
	"github.com/kakoc/hydo/sdk/go/hydoassert"
	"github.com/kakoc/hydo/sdk/go/hydohist"
)

var acked = hydoassert.Always("kv.acked_durable", "every acknowledged write reads back with its value")
var back = hydoassert.Eventually("kv.back", "the server answers within 5 s", 5*time.Second)
var lostReply = hydoassert.Buggify("kv.put.lost_reply", "the write lands but the reply is lost")

func main() {
	hydoassert.Catalog()                                   // once, early: the HYDO-ASSERT-DECL lines
	acked.Check(got == want, hydoassert.D{"key": k, "got": got})
	if lostReply.Fire(hydoassert.D{"key": k}) { return }
	back.Await(func() bool { return ping() == nil }, 100*time.Millisecond, nil)
	hydoassert.SetupComplete()                             // HYDO-SETUP-COMPLETE

	kv := hydohist.New("kv")
	op := kv.Invoke(3, "write", "k1", "v1")                // process 3, before the request
	if err := put("k1", "v1"); err == nil { op.Ok("k1", "v1") } else { op.Info() }
}
```

Each package's documentation (`go doc github.com/kakoc/hydo/sdk/go/hydoassert`) describes every
call, the records and the environment variables (`HYDO_ASSERT_OUT`, `HYDO_BUGGIFY`,
`HYDO_BUGGIFY_FILE`, `HYDO_HIST_OUT`).

## Names

The packages are `hydoassert` and `hydohist`, not `hydo-assert`: a Go identifier cannot contain
a hyphen, and Go's convention is that a package's name is its directory's name. The registry
names of the other SDKs are `hydo-assert` (crates.io, PyPI and npm), and their names in code are
`hydo_assert` (Rust and Python) or the package `hydo-assert` (JavaScript); so the code-level
name, "hydo assert", is the same in every language, spelled as each language allows.

## Two copies: this module and the frozen copy in `hydo/stack`

The same code lives in two places:

- **`sdk/go` (this module, `github.com/kakoc/hydo/sdk/go`)**: the public module that users
  import.
- **`hydo/stack/hydoassert` and `hydo/stack/hydohist`** (inside the module `hydostack`): a
  frozen copy. The order service of the stack example and the bug benchmark imports it, and
  that service's compiled bytes are inside recorded guest images. Changing that module's path or
  those files would change the order service's bytes and move recorded fingerprints, so the
  copy there stays as it is.

The test `cargo test -p hydo-release --test sdks` (in the repository's Cargo workspace) fails
when the two copies differ in anything but these documented differences. Exactly these may
differ, and nothing else (a file on one side only fails too, except the shell SDK):

1. Three lines of `hydoassert/cases_test.go`, each exactly once in each copy:

   | `hydo/stack` | `sdk/go` | why |
   |---|---|---|
   | `"hydostack/hydoassert"`, `"hydostack/hydohist"` | `"github.com/kakoc/hydo/sdk/go/hydoassert"`, `.../hydohist` | the module path |
   | `const casesDir = "../../../sdk/cases"` | `const casesDir = "../../cases"` | where the shared cases are from the package directory |
   | `(in hydo/stack/hydoassert)` | `(in sdk/go)` | the comment that says where to run the tests |

2. Seven identifiers in every file of `hydoassert` (Milestone 163): the public module keeps
   unexported what no call of its API takes or returns, so the assertion type and its
   constants are lower case here and exported in the frozen copy:

   | `hydo/stack` | `sdk/go` |
   |---|---|
   | `Type` | `kind` |
   | `TAlways`, `TAlwaysOrUnreachable`, `TSometimes`, `TReachable`, `TUnreachable`, `TEventually` | `tAlways`, `tAlwaysOrUnreachable`, `tSometimes`, `tReachable`, `tUnreachable`, `tEventually` |

   The test maps every whole-word occurrence of the `sdk/go` names back to the frozen names
   before it compares the files byte for byte, so the copies may differ only by the rename
   (also in comments). It also fails when a frozen name is still a word in `sdk/go` (the rename
   must be complete, so the public module never exports them) and when an `sdk/go` name is a
   word in `hydo/stack` (so the mapping stays one to one).

The public API is therefore: `Always`, `AlwaysOrUnreachable`, `Sometimes`, `Reachable`,
`Unreachable` (an `*Assertion` with `Check`, `Hit`, `Cmp`), `Eventually` (a `*Liveness` with
`Await` and `Begin`, a `*Watch` with `Done` and `Expire`), `Buggify` (a `*Point` with `Fire`),
`Catalog`, `SetupComplete` and the details type `D`; and in `hydohist` `New`, `History`
(`Invoke`, `Record`), `Op` (`Ok`, `Fail`, `Info`), `Nil` and the token functions `Enc`, `Ver`,
`ValOrNil`, `List`, `Append`, `Read`, `ReadResult`. A comparison has one form, `Cmp` with its
operator, as in the other SDKs.

`hydo/stack/hydoassert/hydo_assert.sh` (the shell SDK) is not part of this module. A change to the
Go SDK is made in both places in one commit; a change to the frozen copy changes the order
service's bytes, so it needs a re-baseline of the images that contain it (docs/REBASELINE.md).

## Tests

```sh
go vet ./... && go test ./...           # in sdk/go
GOTOOLCHAIN=go1.26.5 go test ./...      # the Go of the guest images: also the invalid-UTF-8 cases
```

`hydoassert/cases_test.go` runs every shared case of `sdk/cases` (written by the Rust SDK's tests,
the reference; [`sdk/rust/README.md`](../rust/README.md), "The shared cases") and compares the
bytes this SDK writes. Go 1.27 and later write invalid UTF-8 as a raw U+FFFD instead of `�`,
so with them the cases that need invalid UTF-8 are skipped (and reported).

## Publishing

Go has no registry upload: a version is a git tag on the repository, `sdk/go/v<version>` for a
module in the `sdk/go` directory (for example `sdk/go/v0.1.0`), and the Go module proxy fetches
it from GitHub on the first `go get`. A tag, once fetched, is cached by the proxy for good: a
release cannot be replaced, only retracted with a `retract` directive in a later version.
[`docs/PUBLISHING.md`](../../docs/PUBLISHING.md) has the steps.

## License

Apache-2.0 ([`LICENSE`](LICENSE), a copy of the repository's, so that the module's zip carries
it; the test above checks that the copy is the same).
