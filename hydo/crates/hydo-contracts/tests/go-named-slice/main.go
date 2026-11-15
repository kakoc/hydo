// A named slice type in a contract (Milestone 260): Go's debug information names such a slice
// `main.words`, not `[]uint`, and the words of a math/big.Int are the named slice `math/big.nat`.
// The test types.rs builds this program and compiles its contracts.
package main

import (
	"fmt"
	"math/big"
	"os"
)

type words []uint

// hydo: ensures len(w) == 0 || w[0] > 0
//
//go:noinline
func first(w words) uint {
	if len(w) == 0 {
		return 0
	}
	return w[0]
}

// hydo: ensures !b.neg || len(b.abs) == 0 || b.abs[0] <= 5000
//
//go:noinline
func floor(b *big.Int) int {
	return b.Sign()
}

func main() {
	fmt.Println(first(words{uint(len(os.Args))}), floor(big.NewInt(-3)))
}
