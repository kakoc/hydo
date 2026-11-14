//go:build !fixed

package main

import (
	"net/http"
	"strings"
)

// The planted bug: only the first try carries the idempotency key. The retry path builds a new
// request and copies the body and the content type, not the key. A create that took effect but
// whose answer was lost is then paid out again by the next try.
func newRequest(body, key string, try int) *http.Request {
	req, _ := http.NewRequest("POST", api, strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	if try == 1 {
		req.Header.Set("Idempotency-Key", key)
	}
	return req
}
