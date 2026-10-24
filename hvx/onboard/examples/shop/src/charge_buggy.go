//go:build !fixed

package main

import (
	"crypto/rand"
	"encoding/hex"
)

// charge: the PLANTED BUG. Each attempt gets a fresh request id as its idempotency key, so a
// retry after a timeout (the provider took the money but its answer was lost) is a new charge
// to the provider: the customer pays twice.
func charge(o *order) (*answer, error) {
	return withRetries(payments+"/v1/charges", func(int) string {
		b := make([]byte, 8)
		rand.Read(b)
		return "req-" + hex.EncodeToString(b)
	}, map[string]any{"order": ref(o), "amount": o.amount})
}
