//go:build fixed

package main

import (
	"net/http"
	"strings"
)

// The fix: every try of a create carries the order's idempotency key, so the provider answers a
// retry with the first payout and pays nothing more.
func newRequest(body, key string, try int) *http.Request {
	req, _ := http.NewRequest("POST", api, strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Idempotency-Key", key)
	return req
}
