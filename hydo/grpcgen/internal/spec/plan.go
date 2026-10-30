// Package spec is the plan the generator writes and the guest client reads: which RPCs to
// drive, how to fill their requests (hints), how to treat their outcomes. It is plain JSON
// (grpc/plan.json next to the descriptor set grpc/desc.pb) so the client needs no YAML parser.
package spec

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
)

// Format is the plan format version this code writes and reads.
const Format = 1

// Plan is everything the guest client needs besides the descriptors.
type Plan struct {
	Format     int        `json:"format"`
	Target     string     `json:"target"`      // host:port of the server (default for every RPC)
	DeadlineMs int        `json:"deadline_ms"` // per-call deadline (default for every RPC)
	Seed       uint64     `json:"seed"`        // default seed (HYDO_GRPC_SEED etc. override it)
	Steps      Steps      `json:"steps"`       // the numbered-steps mode (`hydo-grpc steps`)
	Drive      Drive      `json:"drive"`       // the composer mode (`hydo-grpc drive <rpc>`)
	Defaults   Defaults   `json:"defaults"`
	Record     Record     `json:"record"`
	RPCs       []RPC      `json:"rpcs"`
	Scenarios  []Scenario `json:"scenarios,omitempty"`
	Sources    []string   `json:"sources"` // the .proto files / reflection target the plan came from
}

// Steps configures `hydo-grpc steps FROM TO`: every step runs Calls operations on each of
// Concurrency workers (in parallel), chosen by weight.
type Steps struct {
	Calls       int `json:"calls"`
	Concurrency int `json:"concurrency"`
}

// Drive configures `hydo-grpc drive <rpc>` (one composer command): Calls operations per run.
type Drive struct {
	Calls int `json:"calls"`
}

// Record says where call records go (package record, HYDO-HIST lines): a file in the guest (full bodies) and
// the console: off, brief (no bodies; the default, so many calls fit in the 64 KiB console)
// or full.
type Record struct {
	Console string `json:"console"`
	History string `json:"history"` // the HYDO-HIST history name (default "grpc")
}

// Defaults are the generator's value ranges where a field has no hint.
type Defaults struct {
	StringLen    [2]int  `json:"string_len"`
	BytesLen     [2]int  `json:"bytes_len"`
	RepeatedLen  [2]int  `json:"repeated_len"`
	MapLen       [2]int  `json:"map_len"`
	Depth        int     `json:"depth"`         // nested messages deeper than this are left unset
	MessageProb  float64 `json:"message_prob"`  // a singular message field is set with this probability
	OptionalProb float64 `json:"optional_prob"` // a proto3 `optional` / proto2 optional scalar is set ...
	OneofNone    float64 `json:"oneof_none"`    // probability that a oneof is left empty
}

// RPC is one method to drive.
type RPC struct {
	Name            string               `json:"name"`  // "pkg.Service/Method"
	Path            string               `json:"path"`  // "/pkg.Service/Method" (the HTTP/2 path)
	Short           string               `json:"short"` // command name: "create" or "orders_create"
	Input           string               `json:"input"`
	Output          string               `json:"output"`
	ClientStreaming bool                 `json:"client_streaming,omitempty"`
	ServerStreaming bool                 `json:"server_streaming,omitempty"`
	Target          string               `json:"target,omitempty"`
	Weight          int                  `json:"weight"` // relative frequency; 0 = never drawn
	DeadlineMs      int                  `json:"deadline_ms"`
	Retry           Retry                `json:"retry"`
	OutcomeID       string               `json:"outcome_id,omitempty"` // request field -> HYDO_ACKED/HYDO_OUTCOMES
	Classify        map[string]string    `json:"classify,omitempty"`   // gRPC code name -> ok|fail|info
	Fields          map[string]FieldHint `json:"fields,omitempty"`     // request field path -> hint
	Oneofs          map[string][]string  `json:"oneofs,omitempty"`     // oneof path -> allowed members
	StreamLen       [2]int               `json:"stream_len"`           // client-streaming: messages sent
	Needs           []string             `json:"needs,omitempty"`      // pools that must be non-empty (derived from `from`)
}

// Retry says what to do after an unknown outcome.
type Retry struct {
	Attempts  int  `json:"attempts"`   // extra attempts right away, with the SAME request (0 = none)
	BackoffMs int  `json:"backoff_ms"` // pause before each extra attempt
	AtEnd     bool `json:"at_end"`     // `hydo-grpc retry` re-sends it after the faults heal ...
	EndTries  int  `json:"end_tries"`  // ... with up to this many attempts (default: 1 + Attempts)
}

// FieldHint constrains one request field. Values are kept as strings and parsed per field type.
type FieldHint struct {
	Value  *string  `json:"value,omitempty"`  // always this value
	Choice []string `json:"choice,omitempty"` // one of these (enum: value names)
	Range  []string `json:"range,omitempty"`  // [lo, hi] inclusive (numbers)
	Len    []int    `json:"len,omitempty"`    // [lo, hi]: string/bytes length, repeated/map size
	Unique bool     `json:"unique,omitempty"` // a value no other operation of this run gets
	Prefix string   `json:"prefix,omitempty"` // strings: prepended
	Format string   `json:"format,omitempty"` // strings: alnum (default) lower digits hex uuid
	Omit   bool     `json:"omit,omitempty"`   // never set
	Prob   *float64 `json:"prob,omitempty"`   // presence probability (optional/message fields)
	From   *From    `json:"from,omitempty"`   // take the value from an earlier call
}

// From names a field of an earlier call whose values feed this one.
type From struct {
	RPC      string `json:"rpc"`      // full method name
	Side     string `json:"side"`     // request | response
	Path     string `json:"path"`     // field path in that message
	Pick     string `json:"pick"`     // random (default) | latest
	Fallback string `json:"fallback"` // skip (default: the call is not made) | generate
}

// Pool is the name of the value pool a From reads.
func (f *From) Pool() string { return f.RPC + "." + f.Side + "." + f.Path }

// Scenario is a fixed sequence of calls run by one worker, values flowing through `from`.
type Scenario struct {
	Name   string   `json:"name"`
	Weight int      `json:"weight"`
	Calls  []string `json:"calls"` // full method names
}

// FieldPaths returns the hinted paths of an RPC in a fixed order.
func (r *RPC) FieldPaths() []string {
	ps := make([]string, 0, len(r.Fields))
	for p := range r.Fields {
		ps = append(ps, p)
	}
	sort.Strings(ps)
	return ps
}

// Find returns the RPC by full name, short name or method name.
func (p *Plan) Find(name string) (*RPC, error) {
	var hits []*RPC
	for i := range p.RPCs {
		r := &p.RPCs[i]
		if r.Name == name || r.Short == name || r.Path == name {
			return r, nil
		}
		if methodName(r.Name) == name {
			hits = append(hits, r)
		}
	}
	if len(hits) == 1 {
		return hits[0], nil
	}
	if len(hits) > 1 {
		return nil, fmt.Errorf("rpc %q is ambiguous (%s, %s, ...): use Service/Method", name, hits[0].Name, hits[1].Name)
	}
	return nil, fmt.Errorf("no rpc %q in the plan", name)
}

func methodName(full string) string {
	for i := len(full) - 1; i >= 0; i-- {
		if full[i] == '/' {
			return full[i+1:]
		}
	}
	return full
}

// Load reads a plan file.
func Load(path string) (*Plan, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var p Plan
	if err := json.Unmarshal(b, &p); err != nil {
		return nil, fmt.Errorf("%s: %v", path, err)
	}
	if p.Format != Format {
		return nil, fmt.Errorf("%s: plan format %d, this client reads %d", path, p.Format, Format)
	}
	return &p, nil
}

// Marshal writes a plan as stable, indented JSON (encoding/json sorts map keys).
func (p *Plan) Marshal() ([]byte, error) {
	b, err := json.MarshalIndent(p, "", "  ")
	if err != nil {
		return nil, err
	}
	return append(b, '\n'), nil
}

// DefaultDefaults are the value ranges used where the hints say nothing.
func DefaultDefaults() Defaults {
	return Defaults{
		StringLen: [2]int{1, 10}, BytesLen: [2]int{0, 16}, RepeatedLen: [2]int{0, 3}, MapLen: [2]int{0, 2},
		Depth: 4, MessageProb: 0.85, OptionalProb: 0.7, OneofNone: 0,
	}
}
