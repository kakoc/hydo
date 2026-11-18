// The error paths of Milestone 306's test (tests/paths.rs): the line numbers are the test's.
package main

import (
	"errors"
	"fmt"
	"os"
)

//go:noinline
func load(n int) (int, error) {
	v, err := parse(n)
	if err != nil {
		return 0, fmt.Errorf("load %d: %w", n, err)
	}
	return v, nil
}

//go:noinline
func retryLoad(n int) int {
	for i := 0; i < 3; i++ {
		v, err := load(n + i)
		if err != nil {
			fmt.Println("retry", err)
			continue
		}
		return v
	}
	// hydo: error path
	fmt.Println("gave up")
	return -1
}

//go:noinline
func parse(n int) (int, error) {
	if n%2 == 1 {
		return 0, errors.New("odd")
	}
	return n / 2, nil
}

// hydo: no error path
func main() {
	if _, err := os.Stat("/"); err != nil {
		os.Exit(1)
	}
	fmt.Println(retryLoad(len(os.Args)))
}
