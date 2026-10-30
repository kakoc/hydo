package outcome

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestClassify(t *testing.T) {
	cases := []struct {
		err  error
		want Kind
		code codes.Code
	}{
		{nil, OK, codes.OK},
		{status.Error(codes.InvalidArgument, "x"), Fail, codes.InvalidArgument},
		{status.Error(codes.NotFound, "x"), Fail, codes.NotFound},
		{status.Error(codes.AlreadyExists, "x"), Fail, codes.AlreadyExists},
		{status.Error(codes.PermissionDenied, "x"), Fail, codes.PermissionDenied},
		{status.Error(codes.Unauthenticated, "x"), Fail, codes.Unauthenticated},
		{status.Error(codes.FailedPrecondition, "x"), Fail, codes.FailedPrecondition},
		{status.Error(codes.OutOfRange, "x"), Fail, codes.OutOfRange},
		{status.Error(codes.Aborted, "x"), Fail, codes.Aborted},
		{status.Error(codes.Unimplemented, "x"), Fail, codes.Unimplemented},
		{status.Error(codes.DeadlineExceeded, "x"), Unknown, codes.DeadlineExceeded},
		{status.Error(codes.Unavailable, "connection reset by peer"), Unknown, codes.Unavailable},
		{status.Error(codes.Canceled, "x"), Unknown, codes.Canceled},
		{status.Error(codes.Internal, "x"), Unknown, codes.Internal},
		{status.Error(codes.Unknown, "x"), Unknown, codes.Unknown},
		{status.Error(codes.DataLoss, "x"), Unknown, codes.DataLoss},
		{status.Error(codes.ResourceExhausted, "x"), Unknown, codes.ResourceExhausted},
		{context.DeadlineExceeded, Unknown, codes.DeadlineExceeded},
		{fmt.Errorf("wrapped: %w", context.Canceled), Unknown, codes.Canceled},
		{errors.New("read tcp: connection reset by peer"), Unknown, codes.Unknown},
	}
	for _, c := range cases {
		r := Classify(c.err, nil)
		if r.Kind != c.want || r.Code != c.code {
			t.Errorf("%v: got %s/%s, want %s/%s", c.err, r.Kind, CodeName(r.Code), c.want, CodeName(c.code))
		}
	}
	// every code has a default
	for c := codes.OK; c <= codes.Unauthenticated; c++ {
		if _, ok := Defaults[c]; !ok {
			t.Errorf("no default for %s", CodeName(c))
		}
		if back, ok := ParseCode(CodeName(c)); !ok || back != c {
			t.Errorf("code name %s does not parse back", CodeName(c))
		}
	}
}

func TestOverrides(t *testing.T) {
	o := map[string]string{"UNAVAILABLE": "fail", "ABORTED": "info"}
	if k := Classify(status.Error(codes.Unavailable, "x"), o).Kind; k != Fail {
		t.Fatalf("override UNAVAILABLE=fail ignored: %s", k)
	}
	if k := Classify(status.Error(codes.Aborted, "x"), o).Kind; k != Unknown {
		t.Fatalf("override ABORTED=info ignored: %s", k)
	}
	if k := Classify(nil, o).Kind; k != OK {
		t.Fatalf("OK changed: %s", k)
	}
	if OK.Word() != "ack" || Fail.Word() != "failed" || Unknown.Word() != "unknown" {
		t.Fatal("HYDO_OUTCOMES words")
	}
}
