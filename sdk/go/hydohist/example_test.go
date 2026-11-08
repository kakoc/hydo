// The examples of the package documentation (pkg.go.dev); the module's README shows pieces of
// them (hydoassert's TestReadme checks that). This file exists only in the public module
// (sdk/go), not in the frozen copy in hydo/stack.

package hydohist_test

import (
	"fmt"

	"github.com/kakoc/hydo/sdk/go/hydohist"
)

// Record an operation history: the invocation before the request, the outcome after it.
func Example() {
	kv := hydohist.New("kv")
	write := kv.Invoke(3, "write", "k1", hydohist.Enc("v1")) // client 3, before the request
	// ... send the request, then record the outcome:
	write.Ok() // it happened; Fail(): it did not; Info(): the outcome is unknown
	read := kv.Invoke(3, "read", "k1")
	read.Ok("k1", hydohist.ValOrNil("v1", true))
	fmt.Println("recorded")
	// Output: recorded
}

// Values become one token each.
func ExampleEnc() {
	fmt.Println(hydohist.Enc("hello world"), hydohist.Enc(""), hydohist.Enc("nil"))
	// Output: hello%20world % %6Eil
}

// A value with the version the store assigned it.
func ExampleVer() {
	fmt.Println(hydohist.Ver("v1", 83))
	// Output: v1@83
}

// An absent value is Nil.
func ExampleValOrNil() {
	fmt.Println(hydohist.ValOrNil("v1", true), hydohist.ValOrNil("", false))
	// Output: v1 nil
}

// The micro-ops of a list-append transaction, and a list value.
func ExampleAppend() {
	fmt.Println(hydohist.Append("x", "1"), hydohist.Read("y"), hydohist.ReadResult("y", "1", "2"), hydohist.List("1", "2"))
	// Output: a:x:1 r:y r:y:[1,2] [1,2]
}
