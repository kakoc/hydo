//go:build fixed

package main

// charge: the fix. The idempotency key belongs to the order, not to the attempt, so every retry
// of the same charge carries the same key and the provider performs it at most once.
func charge(o *order) (*answer, error) {
	key := "charge-" + ref(o)
	return withRetries(payments+"/v1/charges", func(int) string { return key },
		map[string]any{"order": ref(o), "amount": o.amount})
}
