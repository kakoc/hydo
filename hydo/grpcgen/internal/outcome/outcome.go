// Package outcome classifies the result of one gRPC call as ok, a definite failure, or an
// unknown outcome -- the distinction every check of a system under faults depends on: a
// definite failure promises the request had no effect; an unknown outcome means it may or may
// not have taken effect (Jepsen's :fail and :info).
package outcome

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// Kind of an outcome.
type Kind string

const (
	OK      Kind = "ok"   // the call succeeded
	Fail    Kind = "fail" // definitely not applied
	Unknown Kind = "info" // may or may not have been applied
)

// Word is the word main's HYDO_OUTCOMES file uses for a kind (MANIFEST.md, "Variables").
func (k Kind) Word() string {
	switch k {
	case OK:
		return "ack"
	case Fail:
		return "failed"
	}
	return "unknown"
}

// Defaults maps every gRPC status code to a kind.
//
// Definite failures are the codes a server returns when it rejected a request before doing
// anything: bad input, a missing or existing entity, a failed precondition, an auth failure,
// an aborted transaction, an unimplemented method. Everything that can happen after a request
// left the client is unknown: the deadline passed (the server may still run it), the
// connection failed or was reset, the call was cancelled, the server crashed half-way
// (INTERNAL, UNKNOWN, DATA_LOSS), or it was throttled (RESOURCE_EXHAUSTED: some servers throttle
// after partial work). Per-RPC hints can override a code (`classify:`), for an API that
// documents a stronger promise.
var Defaults = map[codes.Code]Kind{
	codes.OK:                 OK,
	codes.InvalidArgument:    Fail,
	codes.NotFound:           Fail,
	codes.AlreadyExists:      Fail,
	codes.PermissionDenied:   Fail,
	codes.Unauthenticated:    Fail,
	codes.FailedPrecondition: Fail,
	codes.OutOfRange:         Fail,
	codes.Aborted:            Fail,
	codes.Unimplemented:      Fail,
	codes.Canceled:           Unknown,
	codes.Unknown:            Unknown,
	codes.DeadlineExceeded:   Unknown,
	codes.ResourceExhausted:  Unknown,
	codes.Internal:           Unknown,
	codes.Unavailable:        Unknown,
	codes.DataLoss:           Unknown,
}

// Result of one call.
type Result struct {
	Kind Kind
	Code codes.Code
	Msg  string // the status message ("" for OK)
}

// CodeName is the status code's canonical upper-case name (DEADLINE_EXCEEDED, ...).
func CodeName(c codes.Code) string {
	if n, ok := names[c]; ok {
		return n
	}
	return fmt.Sprintf("CODE_%d", uint32(c))
}

// ParseCode reads a canonical code name.
func ParseCode(s string) (codes.Code, bool) {
	for c, n := range names {
		if n == s {
			return c, true
		}
	}
	return 0, false
}

// CodeNames lists the canonical names, sorted.
func CodeNames() []string {
	var ns []string
	for _, n := range names {
		ns = append(ns, n)
	}
	sort.Strings(ns)
	return ns
}

var names = map[codes.Code]string{
	codes.OK: "OK", codes.Canceled: "CANCELLED", codes.Unknown: "UNKNOWN",
	codes.InvalidArgument: "INVALID_ARGUMENT", codes.DeadlineExceeded: "DEADLINE_EXCEEDED",
	codes.NotFound: "NOT_FOUND", codes.AlreadyExists: "ALREADY_EXISTS",
	codes.PermissionDenied: "PERMISSION_DENIED", codes.ResourceExhausted: "RESOURCE_EXHAUSTED",
	codes.FailedPrecondition: "FAILED_PRECONDITION", codes.Aborted: "ABORTED",
	codes.OutOfRange: "OUT_OF_RANGE", codes.Unimplemented: "UNIMPLEMENTED",
	codes.Internal: "INTERNAL", codes.Unavailable: "UNAVAILABLE", codes.DataLoss: "DATA_LOSS",
	codes.Unauthenticated: "UNAUTHENTICATED",
}

// Classify turns a call's error into a Result. overrides maps a code name to "ok", "fail" or
// "info" (an RPC's `classify:` hint). An error that is not a gRPC status (it should not
// happen with grpc-go, which wraps transport errors) is unknown; a context deadline or
// cancellation seen outside the status is DEADLINE_EXCEEDED / CANCELLED.
func Classify(err error, overrides map[string]string) Result {
	var c codes.Code
	msg := ""
	if err != nil {
		if s, ok := status.FromError(err); ok {
			c, msg = s.Code(), s.Message()
		} else if errors.Is(err, context.DeadlineExceeded) {
			c, msg = codes.DeadlineExceeded, err.Error()
		} else if errors.Is(err, context.Canceled) {
			c, msg = codes.Canceled, err.Error()
		} else {
			c, msg = codes.Unknown, err.Error()
		}
	}
	k, ok := Defaults[c]
	if !ok {
		k = Unknown
	}
	if o, ok := overrides[CodeName(c)]; ok {
		k = Kind(o)
	}
	if c == codes.OK && err == nil {
		k = OK
		if o, ok := overrides["OK"]; ok {
			k = Kind(o)
		}
	}
	return Result{Kind: k, Code: c, Msg: strings.TrimSpace(msg)}
}

// ValidOverride checks an override value.
func ValidOverride(v string) bool {
	return v == string(OK) || v == string(Fail) || v == string(Unknown)
}
