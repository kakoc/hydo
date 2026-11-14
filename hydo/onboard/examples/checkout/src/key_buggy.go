//go:build !fixed

package main

import "fmt"

// The planted bug: every try of a charge gets its own idempotency key, as when an application
// wraps its retry loop around an SDK call that makes a new key per call. A charge that took
// effect but whose answer was lost is then charged again by the next try.
func idempotencyKey(order string, try int) string {
	return fmt.Sprintf("charge-%s-try%d", order, try)
}
