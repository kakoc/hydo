package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"regexp"
	"strconv"
	"strings"
)

// Config is /etc/hydo/world.json, compiled by hydo-build from the manifest's fakes: and proxies:.
type Config struct {
	Log     string      `json:"log"`
	Sock    string      `json:"sock"`
	Fakes   []*Endpoint `json:"fakes"`
	Proxies []*Endpoint `json:"proxies"`
}

// Endpoint is one fake service or one fault proxy. Kind "fake" answers itself from its routes;
// kind "proxy" forwards to To (a real service) and only applies the faults.
type Endpoint struct {
	Kind        string   `json:"kind"`
	Name        string   `json:"name"`
	Addr        string   `json:"addr"`
	Port        int      `json:"port"`
	Protocol    string   `json:"protocol"`    // http (HTTP/1.1 + cleartext HTTP/2) | grpc | tcp (proxy only)
	Idempotency string   `json:"idempotency"` // extractor of the request's idempotency key
	Ref         string   `json:"ref"`         // default extractor of the business reference
	To          string   `json:"to"`          // proxy: upstream host:port
	Routes      []*Route `json:"routes"`
}

type Route struct {
	Name       string            `json:"name"`
	Method     string            `json:"method"` // "" = any
	Path       string            `json:"path"`   // exact, /prefix/*, or with {name} segments
	Status     int               `json:"status"` // http answer (fake)
	Code       int               `json:"code"`   // grpc status of the answer (fake)
	Headers    map[string]string `json:"headers"`
	Body       string            `json:"body"`    // http answer body (template)
	Message    string            `json:"message"` // grpc answer message, hex protobuf bytes
	Ref        string            `json:"ref"`
	Effect     *bool             `json:"effect"` // does a request on this route have a side effect
	Idempotent bool              `json:"idempotent"`
	Webhook    *Webhook          `json:"webhook"`
	segs       []string
}

type Webhook struct {
	URL     string            `json:"url"`
	Method  string            `json:"method"`
	Body    string            `json:"body"` // template: {seq} {ref} {idem} {fake} {route} only
	Headers map[string]string `json:"headers"`
	DelayMs int               `json:"delay_ms"`
	Retries int               `json:"retries"`
}

func loadConfig(path string) (*Config, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var c Config
	if err := json.Unmarshal(b, &c); err != nil {
		return nil, fmt.Errorf("%s: %v", path, err)
	}
	for _, e := range c.Fakes {
		e.Kind = "fake"
	}
	for _, e := range c.Proxies {
		e.Kind = "proxy"
	}
	for _, e := range c.all() {
		if e.Protocol == "" {
			e.Protocol = "http"
		}
		if e.Addr == "" {
			e.Addr = "127.0.0.1"
		}
		for _, r := range e.Routes {
			if r.Path != "" && !strings.HasSuffix(r.Path, "*") {
				r.segs = strings.Split(strings.Trim(r.Path, "/"), "/")
			}
			if r.Status == 0 {
				r.Status = 200
			}
			if r.Webhook != nil {
				if r.Webhook.Method == "" {
					r.Webhook.Method = "POST"
				}
				if r.Webhook.Retries == 0 {
					r.Webhook.Retries = 5
				}
			}
		}
	}
	return &c, nil
}

func (c *Config) all() []*Endpoint { return append(append([]*Endpoint{}, c.Fakes...), c.Proxies...) }

func (c *Config) endpoint(name string) *Endpoint {
	for _, e := range c.all() {
		if e.Name == name {
			return e
		}
	}
	return nil
}

func (e *Endpoint) route(name string) *Route {
	for _, r := range e.Routes {
		if r.Name == name {
			return r
		}
	}
	return nil
}

// match: the first route whose method and path fit; path params go into vars ("path:<name>").
func (e *Endpoint) match(method, path string) (*Route, map[string]string) {
	for _, r := range e.Routes {
		if r.Method != "" && !strings.EqualFold(r.Method, method) {
			continue
		}
		if r.Path == "" || r.Path == "*" {
			return r, nil
		}
		if strings.HasSuffix(r.Path, "*") {
			if strings.HasPrefix(path, strings.TrimSuffix(r.Path, "*")) {
				return r, nil
			}
			continue
		}
		ps := strings.Split(strings.Trim(path, "/"), "/")
		if len(ps) != len(r.segs) {
			continue
		}
		vars, ok := map[string]string{}, true
		for i, s := range r.segs {
			if strings.HasPrefix(s, "{") && strings.HasSuffix(s, "}") {
				vars[s[1:len(s)-1]] = ps[i]
			} else if s != ps[i] {
				ok = false
				break
			}
		}
		if ok {
			return r, vars
		}
	}
	return nil, nil
}

// effectful: a request on the route has a side effect in the outside world. Default: every
// method but GET/HEAD/OPTIONS (gRPC: every call).
func (r *Route) effectful(method string, proto string) bool {
	if r != nil && r.Effect != nil {
		return *r.Effect
	}
	if proto == "grpc" {
		return true
	}
	switch strings.ToUpper(method) {
	case "GET", "HEAD", "OPTIONS":
		return false
	}
	return true
}

// ---------------------------------------------------------------- request view + extractors

type reqView struct {
	method, path string
	header       http.Header
	query        map[string][]string
	body         []byte
	msg          []byte // grpc: the first message's bytes
	vars         map[string]string
	js           map[string]any
	jsDone       bool
}

func (v *reqView) json() map[string]any {
	if !v.jsDone {
		v.jsDone = true
		_ = json.Unmarshal(v.body, &v.js)
	}
	return v.js
}

func bodyHash(b []byte) string {
	h := sha256.Sum256(b)
	return hex.EncodeToString(h[:6])
}

// extract evaluates an extractor spec: header:<Name> | json:<a.b> | query:<name> |
// path:<name or 1-based index> | form:<name> | pb:<field number> | body (hash) | "" (none).
func (v *reqView) extract(spec string) string {
	if v == nil || spec == "" {
		return ""
	}
	kind, arg, _ := strings.Cut(spec, ":")
	switch kind {
	case "header":
		return v.header.Get(arg)
	case "query":
		if q := v.query[arg]; len(q) > 0 {
			return q[0]
		}
	case "form":
		if vals, err := parseForm(v.body); err == nil {
			if q := vals[arg]; len(q) > 0 {
				return q[0]
			}
		}
	case "path":
		if s, ok := v.vars[arg]; ok {
			return s
		}
		if n, err := strconv.Atoi(arg); err == nil {
			ps := strings.Split(strings.Trim(v.path, "/"), "/")
			if n >= 1 && n <= len(ps) {
				return ps[n-1]
			}
		}
	case "json":
		var cur any = v.json()
		for _, k := range strings.Split(arg, ".") {
			m, ok := cur.(map[string]any)
			if !ok {
				return ""
			}
			cur = m[k]
		}
		switch x := cur.(type) {
		case string:
			return x
		case float64:
			return strconv.FormatFloat(x, 'f', -1, 64)
		case bool:
			return strconv.FormatBool(x)
		case nil:
			return ""
		default:
			b, _ := json.Marshal(x)
			return string(b)
		}
	case "pb":
		n, err := strconv.Atoi(arg)
		if err == nil {
			return pbField(v.msg, n)
		}
	case "body":
		return bodyHash(v.body)
	}
	return ""
}

// pbField: the first top-level field n of a protobuf message, as a string (length-delimited
// bytes) or a decimal (varint). Enough to name a call by a field of its request.
func pbField(b []byte, n int) string {
	for i := 0; i < len(b); {
		key, k := uvarint(b[i:])
		if k <= 0 {
			return ""
		}
		i += k
		f, wt := int(key>>3), key&7
		switch wt {
		case 0:
			x, k := uvarint(b[i:])
			if k <= 0 {
				return ""
			}
			i += k
			if f == n {
				return strconv.FormatUint(x, 10)
			}
		case 1:
			i += 8
		case 5:
			i += 4
		case 2:
			l, k := uvarint(b[i:])
			if k <= 0 || i+k+int(l) > len(b) {
				return ""
			}
			i += k
			if f == n {
				return string(b[i : i+int(l)])
			}
			i += int(l)
		default:
			return ""
		}
	}
	return ""
}

func uvarint(b []byte) (uint64, int) {
	var x uint64
	for i, c := range b {
		if i == 10 {
			return 0, -1
		}
		x |= uint64(c&0x7f) << (7 * uint(i))
		if c < 0x80 {
			return x, i + 1
		}
	}
	return 0, -1
}

var tmplRE = regexp.MustCompile(`\{(seq|ref|idem|fake|route|json|header|query|path|form|pb)(?::([^{}"]*))?\}`)

// render a template: {seq} {ref} {idem} {fake} {route} plus, with a request, {json:a.b}
// {header:X} {query:x} {path:x} {form:x} {pb:n}. (Single braces: {{name}} is a build parameter.)
func render(t string, ctx map[string]string, v *reqView) string {
	return tmplRE.ReplaceAllStringFunc(t, func(m string) string {
		g := tmplRE.FindStringSubmatch(m)
		if g[2] == "" {
			return ctx[g[1]]
		}
		if v == nil {
			return ""
		}
		return v.extract(g[1] + ":" + g[2])
	})
}

// esc: a value safe for one whitespace-separated, comma-separated field of a record or token
func esc(s string) string {
	if s == "" {
		return "-"
	}
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		if c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || strings.IndexByte("_.:@/+-", c) >= 0 {
			b.WriteByte(c)
		} else {
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	return b.String()
}

func unesc(s string) string {
	if s == "-" {
		return ""
	}
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] == '%' && i+2 < len(s) {
			if x, err := strconv.ParseUint(s[i+1:i+3], 16, 8); err == nil {
				b.WriteByte(byte(x))
				i += 2
				continue
			}
		}
		b.WriteByte(s[i])
	}
	return b.String()
}
