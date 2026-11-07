# hydo/stack: the order service of the stack example

The gRPC order service (`main.go`, `buggify.go`, `proto/`) that the stack example, the stack
flagship images and the bug benchmark build, in the Go module `hydostack`.

## A frozen copy of the Go SDK

`hydoassert/` (the assertion SDK, plus the shell SDK `hydo_assert.sh`) and `hydohist/` (the
history recorder) are a **frozen copy** of hydo's Go SDK. The public module that users import is
`sdk/go` (`github.com/kakoc/hydo/sdk/go/hydoassert` and `.../hydohist`; `sdk/go/README.md`).

The copy stays here because the order service's compiled bytes are inside recorded guest images
(the stack images, the pinned reproducers, the benchmark's images): moving these packages,
renaming the module or editing these files would change the order service's binary and move
recorded fingerprints. So:

- do not change the module path `hydostack` or the files in `hydoassert/` and `hydohist/` for
  the sake of the public SDK;
- the test `cargo test -p hydo-release --test sdks` fails when `sdk/go` and this copy differ in
  anything but the documented differences (`sdk/go/README.md`, "Two copies", lists them): the
  import path, the shared cases' directory and one comment in `hydoassert/cases_test.go`, and,
  since Milestone 163, seven identifiers that `sdk/go` made unexported (the assertion type
  `Type` and its constants `TAlways`, `TAlwaysOrUnreachable`, `TSometimes`, `TReachable`,
  `TUnreachable`, `TEventually`, which no call of the API takes or returns). This copy keeps the
  exported names: it is not a public API, and changing it would move the recorded fingerprints;
- a real change to the Go SDK is made in both places in one commit, and because it changes the
  order service's bytes, the images that contain it are re-recorded (docs/REBASELINE.md).
