// Package record writes one line per call event in the history recorder's format (HYDO-HIST,
// hydo/HISTORIES.md), under one history name (default "grpc"):
//
//	HYDO-HIST <history> <process> invoke <time> <rpc> <op> <request>
//	HYDO-HIST <history> <process> ok|fail|info <time> <rpc> <op> <CODE> <response-or-message>
//
// <process> is the worker (one operation in flight per process), <time> guest nanoseconds since
// boot (CLOCK_BOOTTIME; deterministic under hydo), <rpc> "pkg.Service/Method" (the operation
// <f>), <op> the operation id (a retry is a new invocation with the same op and request),
// <CODE> the gRPC status name. Bodies are compact protobuf JSON. Every token is percent-encoded:
// bytes outside '!'..'~', '%' and '@' become %XX; the token "%" alone is the empty string.
//
// Lines go to a file (full bodies) and optionally to the guest console (full, or brief:
// without the request and response, so many calls fit in the 64 KiB console).
package record

import (
	"fmt"
	"os"
	"strings"
	"sync"
	"syscall"
)

// Enc percent-encodes one token.
func Enc(s string) string {
	if s == "" {
		return "%"
	}
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		if c <= ' ' || c > '~' || c == '%' || c == '@' {
			fmt.Fprintf(&b, "%%%02X", c)
		} else {
			b.WriteByte(c)
		}
	}
	return b.String()
}

// Dec reverses Enc.
func Dec(s string) (string, error) {
	if s == "%" {
		return "", nil
	}
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] != '%' {
			b.WriteByte(s[i])
			continue
		}
		if i+3 > len(s) {
			return "", fmt.Errorf("bad escape in %q", s)
		}
		var v byte
		if _, err := fmt.Sscanf(s[i+1:i+3], "%02X", &v); err != nil {
			return "", fmt.Errorf("bad escape in %q", s)
		}
		b.WriteByte(v)
		i += 2
	}
	return b.String(), nil
}

// Event is one parsed line.
type Event struct {
	Process string
	Op      string
	Type    string // invoke ok fail info
	Time    string
	RPC     string
	Code    string // "" for invoke
	Body    string // decoded request (invoke) or response/message
}

// Tag starts every record.
const Tag = "HYDO-HIST "

// Parse reads one line of history h (ok=false if it is not such a record).
func Parse(line, h string) (Event, bool) {
	i := strings.Index(line, Tag)
	if i < 0 {
		return Event{}, false
	}
	f := strings.Fields(line[i+len(Tag):])
	if len(f) < 6 || f[0] != h {
		return Event{}, false
	}
	e := Event{Process: f[1], Type: f[2], Time: f[3], RPC: f[4], Op: f[5]}
	rest := f[6:]
	if e.Type != "invoke" {
		if len(rest) == 0 {
			return Event{}, false
		}
		e.Code, rest = rest[0], rest[1:]
	}
	if len(rest) > 0 {
		b, err := Dec(rest[0])
		if err != nil {
			return Event{}, false
		}
		e.Body = b
	}
	return e, true
}

// Console modes.
const (
	ConsoleOff   = "off"
	ConsoleBrief = "brief"
	ConsoleFull  = "full"
)

// Recorder appends events.
type Recorder struct {
	History string // the history name (default "grpc")
	mu      sync.Mutex
	file    *os.File
	console *os.File
	mode    string
}

// Open opens the calls file (path "" = none) and the console per mode (HYDO_GRPC_CONSOLE
// overrides; the device is HYDO_GRPC_CONSOLE_DEV, default /dev/ttyS0, falling back to stderr).
func Open(path, mode string) *Recorder {
	r := &Recorder{mode: mode, History: "grpc"}
	if v := os.Getenv("HYDO_GRPC_CONSOLE"); v != "" {
		r.mode = v
	}
	if path != "" {
		if f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644); err == nil {
			r.file = f
		} else {
			fmt.Fprintf(os.Stderr, "hydo-grpc: cannot open %s: %v\n", path, err)
		}
	}
	if r.mode == ConsoleBrief || r.mode == ConsoleFull {
		dev := os.Getenv("HYDO_GRPC_CONSOLE_DEV")
		if dev == "" {
			dev = "/dev/ttyS0"
		}
		if dev == "stderr" {
			r.console = os.Stderr
		} else if f, err := os.OpenFile(dev, os.O_WRONLY|os.O_APPEND|syscall.O_NOCTTY, 0); err == nil {
			r.console = f
		} else {
			r.console = os.Stderr
		}
	}
	return r
}

// Invoke records the start of an attempt.
func (r *Recorder) Invoke(process, op int64, rpc, request string) {
	head := fmt.Sprintf("%s%s %d invoke %d %s %d", Tag, r.History, process, Now(), rpc, op)
	r.write(head, Enc(request))
}

// Complete records the outcome of an attempt.
func (r *Recorder) Complete(process, op int64, kind, rpc, code, body string) {
	head := fmt.Sprintf("%s%s %d %s %d %s %d %s", Tag, r.History, process, kind, Now(), rpc, op, code)
	r.write(head, Enc(body))
}

func (r *Recorder) write(head, body string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	full := head + " " + body + "\n"
	if r.file != nil {
		r.file.Write([]byte(full)) // one write: O_APPEND keeps lines of concurrent processes whole
	}
	if r.console != nil {
		if r.mode == ConsoleFull {
			r.console.Write([]byte(full))
		} else {
			r.console.Write([]byte(head + "\n"))
		}
	}
}

// Raw writes an arbitrary line to the console sink (assertion records).
func (r *Recorder) Raw(line string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.console != nil {
		r.console.Write([]byte(line + "\n"))
	}
}
