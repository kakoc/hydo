//go:build fixed

package main

// The fix: one idempotency key per order, so that every retry of a charge is the same request
// to the provider, which answers it with the first charge.
func idempotencyKey(order string, try int) string {
	return "charge-" + order
}
