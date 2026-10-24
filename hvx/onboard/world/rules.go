package main

import (
	"fmt"
	"hash/fnv"
	"net/url"
	"strconv"
	"strings"
)

func parseForm(b []byte) (url.Values, error) { return url.ParseQuery(string(b)) }

// Rule: one H= token, switched on by the init for the token's step window.
//
//	H=<target>@<from>-<to>:<rule>[,<rule>...]
//	target  <fake|proxy> or <fake|proxy>.<route>
//	request rules: status=<code> [retry-after=<s>] | delay=<ms> | drop | reset
//	webhook rules: dup | reorder | late=<ms> | lost
//	p=<1..100>      share of the matching requests (or webhooks) the rule acts on (default 100)
//	effect=yes|no   whether the outside world still performs the side effect; default no for
//	                status=, yes for delay/drop/reset (the request arrived, the answer is lost)
type Rule struct {
	ID         int
	Token      string
	Target     string // endpoint name
	Route      string // "" = every route
	Status     int    // http status or (grpc) status code; 0 = none
	RetryAfter string
	DelayMs    int
	Drop       bool
	Reset      bool
	Dup        bool
	Reorder    bool
	LateMs     int
	Lost       bool
	P          int
	Effect     int // -1 default, 0 no, 1 yes
	n          uint64
	release    chan struct{}
}

var grpcCodes = map[string]int{"OK": 0, "CANCELLED": 1, "UNKNOWN": 2, "INVALID_ARGUMENT": 3,
	"DEADLINE_EXCEEDED": 4, "NOT_FOUND": 5, "ALREADY_EXISTS": 6, "PERMISSION_DENIED": 7,
	"RESOURCE_EXHAUSTED": 8, "FAILED_PRECONDITION": 9, "ABORTED": 10, "OUT_OF_RANGE": 11,
	"UNIMPLEMENTED": 12, "INTERNAL": 13, "UNAVAILABLE": 14, "DATA_LOSS": 15, "UNAUTHENTICATED": 16}

func parseRule(id int, token string) (*Rule, error) {
	r := &Rule{ID: id, Token: token, P: 100, Effect: -1, release: make(chan struct{})}
	head, rules, ok := strings.Cut(token, ":")
	if !ok {
		return nil, fmt.Errorf("no ':' in %q", token)
	}
	target, _, _ := strings.Cut(head, "@") // the window is the init's business
	r.Target, r.Route, _ = strings.Cut(target, ".")
	if r.Target == "" {
		return nil, fmt.Errorf("no target in %q", token)
	}
	for _, kv := range strings.Split(rules, ",") {
		k, v, _ := strings.Cut(kv, "=")
		num := func() (int, error) {
			n, err := strconv.Atoi(v)
			if err != nil || n < 0 {
				return 0, fmt.Errorf("%s=%q is not a number", k, v)
			}
			return n, nil
		}
		var err error
		switch k {
		case "status":
			if c, ok := grpcCodes[strings.ToUpper(v)]; ok {
				r.Status = c
			} else {
				r.Status, err = num()
			}
		case "retry-after":
			r.RetryAfter = v
		case "delay":
			r.DelayMs, err = num()
		case "late":
			r.LateMs, err = num()
		case "p":
			r.P, err = num()
		case "drop":
			r.Drop = true
		case "reset":
			r.Reset = true
		case "dup":
			r.Dup = true
		case "reorder":
			r.Reorder = true
		case "lost":
			r.Lost = true
		case "effect":
			switch v {
			case "yes":
				r.Effect = 1
			case "no":
				r.Effect = 0
			default:
				err = fmt.Errorf("effect=%q: yes or no", v)
			}
		case "":
		default:
			err = fmt.Errorf("unknown rule %q", k)
		}
		if err != nil {
			return nil, err
		}
	}
	return r, nil
}

func (r *Rule) requestRule() bool { return r.Status > 0 || r.DelayMs > 0 || r.Drop || r.Reset }
func (r *Rule) hookRule() bool    { return r.Dup || r.Reorder || r.LateMs > 0 || r.Lost }

func (r *Rule) matches(ep, route string) bool {
	return r.Target == ep && (r.Route == "" || r.Route == route)
}

// applied: does the outside world perform the request's side effect under this rule?
func (r *Rule) applied() bool {
	if r.Effect >= 0 {
		return r.Effect == 1
	}
	return r.Status == 0 // a plain error answer by default means "rejected before acting"
}

func mix64(x uint64) uint64 {
	x += 0x9E3779B97F4A7C15
	x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9
	x = (x ^ (x >> 27)) * 0x94D049BB133111EB
	return x ^ (x >> 31)
}

// fires: the rule's decision for the next matching request (or webhook): a pure function of the
// token text and how many matching requests the rule has seen. The caller holds the world lock.
func (r *Rule) fires() bool {
	h := fnv.New64a()
	h.Write([]byte(r.Token))
	k := r.n
	r.n++
	return mix64(h.Sum64()+k)%100 < uint64(r.P)
}

// outcome: what a request rule did, for the record
func (r *Rule) outcome(proto string) string {
	var p []string
	if r.DelayMs > 0 {
		p = append(p, fmt.Sprintf("delay%d", r.DelayMs))
	}
	switch {
	case r.Drop:
		p = append(p, "drop")
	case r.Reset:
		p = append(p, "reset")
	case r.Status > 0:
		p = append(p, fmt.Sprintf("status%d", r.Status))
	}
	return strings.Join(p, "+")
}
