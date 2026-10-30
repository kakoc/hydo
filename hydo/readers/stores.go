package main

import (
	"bufio"
	"bytes"
	"context"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"time"

	_ "github.com/go-sql-driver/mysql"
	_ "github.com/lib/pq"
	"github.com/twmb/franz-go/pkg/kadm"
	"github.com/twmb/franz-go/pkg/kgo"
)

// ---------------------------------------------------------------- SQL

// mysqlDSN accepts the driver's own form (user:pw@tcp(host:port)/db?...) or a URL
// (mysql://user:pw@host:port/db?...), and adds connect/read timeouts if absent.
func mysqlDSN(dsn string, t time.Duration) (string, error) {
	if strings.HasPrefix(dsn, "mysql://") {
		u, err := url.Parse(dsn)
		if err != nil {
			return "", usagef("mysql dsn %q: %v", dsn, err)
		}
		host := u.Host
		if !strings.Contains(host, ":") {
			host += ":3306"
		}
		ui := ""
		if u.User != nil {
			ui = u.User.Username()
			if p, ok := u.User.Password(); ok {
				ui += ":" + p
			}
			ui += "@"
		}
		dsn = fmt.Sprintf("%stcp(%s)%s", ui, host, u.Path)
		if dsn[len(dsn)-1] != '/' && u.Path == "" {
			dsn += "/"
		}
		if u.RawQuery != "" {
			dsn += "?" + u.RawQuery
		}
	}
	sec := fmt.Sprintf("%ds", int(t.Seconds()+0.999))
	for _, p := range []string{"timeout", "readTimeout", "writeTimeout"} {
		if !strings.Contains(dsn, p+"=") {
			if strings.Contains(dsn, "?") {
				dsn += "&"
			} else {
				dsn += "?"
			}
			dsn += p + "=" + sec
		}
	}
	return dsn, nil
}

func postgresDSN(dsn string, t time.Duration) string {
	sec := strconv.Itoa(int(t.Seconds() + 0.999))
	if strings.Contains(dsn, "connect_timeout") {
		return dsn
	}
	if strings.HasPrefix(dsn, "postgres://") || strings.HasPrefix(dsn, "postgresql://") {
		if strings.Contains(dsn, "?") {
			return dsn + "&connect_timeout=" + sec
		}
		return dsn + "?connect_timeout=" + sec
	}
	return dsn + " connect_timeout=" + sec
}

// sqlValue turns a driver value into a JSON-able one: text as a string, NULL as nil.
func sqlValue(v any) any {
	switch x := v.(type) {
	case nil:
		return nil
	case []byte:
		return string(x)
	case string:
		return x
	case int64:
		return strconv.FormatInt(x, 10)
	case float64:
		return strconv.FormatFloat(x, 'g', -1, 64)
	case bool:
		if x {
			return "true"
		}
		return "false"
	case time.Time:
		return x.UTC().Format(time.RFC3339Nano)
	}
	return fmt.Sprint(v)
}

func readSQL(store, dsn, query string, o readOpts) ([]Record, error) {
	driver := store
	if store == "mysql" {
		d, err := mysqlDSN(dsn, o.timeout)
		if err != nil {
			return nil, err
		}
		dsn = d
	} else {
		driver = "postgres"
		dsn = postgresDSN(dsn, o.timeout)
	}
	db, err := sql.Open(driver, dsn)
	if err != nil {
		return nil, usagef("%s dsn: %v", store, err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	ctx, cancel := context.WithTimeout(context.Background(), o.timeout)
	defer cancel()
	rows, err := db.QueryContext(ctx, query)
	if err != nil {
		return nil, cannot(err)
	}
	defer rows.Close()
	cols, err := rows.Columns()
	if err != nil {
		return nil, cannot(err)
	}
	var out []Record
	for rows.Next() {
		vals := make([]any, len(cols))
		ptrs := make([]any, len(cols))
		for i := range vals {
			ptrs[i] = &vals[i]
		}
		if err := rows.Scan(ptrs...); err != nil {
			return nil, cannot(err)
		}
		r := Record{}
		for i, c := range cols {
			r[c] = sqlValue(vals[i])
		}
		out = append(out, r)
	}
	if err := rows.Err(); err != nil {
		return nil, cannot(err)
	}
	return out, nil
}

// ---------------------------------------------------------------- Kafka

// readKafka reads every record of every partition of a topic, from the log start offset
// to the high watermark as they are when the read starts, in (partition, offset) order.
// It never creates the topic (franz-go does not ask the broker to auto-create on
// metadata requests unless told to). A missing topic is "could not read".
func readKafka(bootstrap, topic string, o readOpts) ([]Record, error) {
	ctx, cancel := context.WithTimeout(context.Background(), o.timeout)
	defer cancel()
	seeds := strings.Split(bootstrap, ",")
	base := []kgo.Opt{kgo.SeedBrokers(seeds...), kgo.DialTimeout(o.timeout),
		kgo.RetryTimeout(o.timeout), kgo.MetadataMinAge(100 * time.Millisecond)}
	acl, err := kgo.NewClient(base...)
	if err != nil {
		return nil, usagef("kafka: %v", err)
	}
	defer acl.Close()
	adm := kadm.NewClient(acl)
	ends, err := adm.ListEndOffsets(ctx, topic)
	if err != nil {
		return nil, cannotf("kafka %s: end offsets: %v", topic, err)
	}
	if err := ends.Error(); err != nil {
		return nil, cannotf("kafka %s: end offsets: %v", topic, err)
	}
	starts, err := adm.ListStartOffsets(ctx, topic)
	if err != nil {
		return nil, cannotf("kafka %s: start offsets: %v", topic, err)
	}
	if err := starts.Error(); err != nil {
		return nil, cannotf("kafka %s: start offsets: %v", topic, err)
	}
	if _, ok := ends[topic]; !ok || len(ends[topic]) == 0 {
		return nil, cannotf("kafka: topic %s does not exist", topic)
	}
	end := map[int32]int64{}
	next := map[int32]int64{}
	assign := map[int32]kgo.Offset{}
	var parts []int32
	for p, lo := range ends[topic] {
		parts = append(parts, p)
		end[p] = lo.Offset
		s := starts[topic][p].Offset
		next[p] = s
		if s < lo.Offset {
			assign[p] = kgo.NewOffset().At(s)
		}
	}
	sort.Slice(parts, func(i, j int) bool { return parts[i] < parts[j] })
	byPart := map[int32][]Record{}
	if len(assign) > 0 {
		cl, err := kgo.NewClient(append(base, kgo.ConsumePartitions(map[string]map[int32]kgo.Offset{topic: assign}),
			kgo.FetchMaxWait(200*time.Millisecond))...)
		if err != nil {
			return nil, usagef("kafka: %v", err)
		}
		defer cl.Close()
		remaining := len(assign)
		done := map[int32]bool{}
		for remaining > 0 {
			fs := cl.PollFetches(ctx)
			if ctx.Err() != nil {
				return nil, cannotf("kafka %s: timed out after reading %d records (%v)", topic, countRecs(byPart), ctx.Err())
			}
			for _, fe := range fs.Errors() {
				if fe.Err == context.DeadlineExceeded || fe.Err == context.Canceled {
					continue
				}
				return nil, cannotf("kafka %s partition %d: %v", fe.Topic, fe.Partition, fe.Err)
			}
			fs.EachRecord(func(r *kgo.Record) {
				p := r.Partition
				if done[p] || r.Offset >= end[p] {
					return
				}
				byPart[p] = append(byPart[p], kafkaRecord(r, o.base64))
				next[p] = r.Offset + 1
				if next[p] >= end[p] {
					done[p] = true
					remaining--
				}
			})
		}
	}
	var out []Record
	for _, p := range parts {
		out = append(out, byPart[p]...)
	}
	return out, nil
}

func countRecs(m map[int32][]Record) int {
	n := 0
	for _, v := range m {
		n += len(v)
	}
	return n
}

func bytesValue(b []byte, b64 bool) any {
	if b == nil {
		return nil
	}
	if b64 {
		return base64.StdEncoding.EncodeToString(b)
	}
	return string(b)
}

func kafkaRecord(r *kgo.Record, b64 bool) Record {
	return Record{
		"partition":    int64(r.Partition),
		"offset":       r.Offset,
		"key":          bytesValue(r.Key, b64),
		"value":        bytesValue(r.Value, b64),
		"timestamp_ms": r.Timestamp.UnixMilli(),
	}
}

// ---------------------------------------------------------------- Redis (RESP2)

type redisErr string

func (e redisErr) Error() string { return string(e) }

type redisConn struct {
	c net.Conn
	r *bufio.Reader
	t time.Duration
}

// dialRedis: addr is host:port or redis://[:password@]host:port[/db].
func dialRedis(addr string, t time.Duration) (*redisConn, error) {
	pass, db := "", ""
	if strings.HasPrefix(addr, "redis://") {
		u, err := url.Parse(addr)
		if err != nil {
			return nil, usagef("redis address %q: %v", addr, err)
		}
		if u.User != nil {
			pass, _ = u.User.Password()
			if pass == "" {
				pass = u.User.Username()
			}
		}
		db = strings.Trim(u.Path, "/")
		addr = u.Host
	}
	if !strings.Contains(addr, ":") {
		addr += ":6379"
	}
	c, err := net.DialTimeout("tcp", addr, t)
	if err != nil {
		return nil, cannot(err)
	}
	rc := &redisConn{c: c, r: bufio.NewReader(c), t: t}
	if pass != "" {
		if _, err := rc.do("AUTH", pass); err != nil {
			c.Close()
			return nil, cannot(err)
		}
	}
	if db != "" {
		if _, err := rc.do("SELECT", db); err != nil {
			c.Close()
			return nil, cannot(err)
		}
	}
	return rc, nil
}

func (rc *redisConn) do(args ...string) (any, error) {
	rc.c.SetDeadline(time.Now().Add(rc.t))
	var b bytes.Buffer
	fmt.Fprintf(&b, "*%d\r\n", len(args))
	for _, a := range args {
		fmt.Fprintf(&b, "$%d\r\n%s\r\n", len(a), a)
	}
	if _, err := rc.c.Write(b.Bytes()); err != nil {
		return nil, err
	}
	v, err := rc.reply()
	if err != nil {
		return nil, err
	}
	if e, ok := v.(redisErr); ok {
		return nil, e
	}
	return v, nil
}

func (rc *redisConn) reply() (any, error) {
	line, err := rc.r.ReadString('\n')
	if err != nil {
		return nil, err
	}
	line = strings.TrimRight(line, "\r\n")
	if line == "" {
		return nil, errors.New("redis: empty reply line")
	}
	switch line[0] {
	case '+':
		return line[1:], nil
	case '-':
		return redisErr(line[1:]), nil
	case ':':
		n, err := strconv.ParseInt(line[1:], 10, 64)
		return n, err
	case '$':
		n, err := strconv.Atoi(line[1:])
		if err != nil {
			return nil, err
		}
		if n < 0 {
			return nil, nil
		}
		buf := make([]byte, n+2)
		if _, err := io.ReadFull(rc.r, buf); err != nil {
			return nil, err
		}
		return string(buf[:n]), nil
	case '*':
		n, err := strconv.Atoi(line[1:])
		if err != nil {
			return nil, err
		}
		if n < 0 {
			return nil, nil
		}
		arr := make([]any, n)
		for i := range arr {
			if arr[i], err = rc.reply(); err != nil {
				return nil, err
			}
			if e, ok := arr[i].(redisErr); ok {
				arr[i] = "ERR " + string(e)
			}
		}
		return arr, nil
	}
	return nil, fmt.Errorf("redis: unexpected reply %q", line)
}

func strs(v any) []string {
	a, _ := v.([]any)
	out := make([]string, 0, len(a))
	for _, x := range a {
		s, _ := x.(string)
		out = append(out, s)
	}
	return out
}

// redisValue reads one key of any type: a string, a {field: value} map, a sorted member
// list (set), a list, or [[member, score], ...] (zset).
func (rc *redisConn) value(key string) (string, any, error) {
	t, err := rc.do("TYPE", key)
	if err != nil {
		return "", nil, err
	}
	typ, _ := t.(string)
	switch typ {
	case "none":
		return typ, nil, nil
	case "string":
		v, err := rc.do("GET", key)
		return typ, v, err
	case "hash":
		v, err := rc.do("HGETALL", key)
		if err != nil {
			return typ, nil, err
		}
		m := map[string]any{}
		s := strs(v)
		for i := 0; i+1 < len(s); i += 2 {
			m[s[i]] = s[i+1]
		}
		return typ, m, nil
	case "set":
		v, err := rc.do("SMEMBERS", key)
		if err != nil {
			return typ, nil, err
		}
		s := strs(v)
		sort.Strings(s)
		return typ, anys(s), nil
	case "list":
		v, err := rc.do("LRANGE", key, "0", "-1")
		return typ, v, err
	case "zset":
		v, err := rc.do("ZRANGE", key, "0", "-1", "WITHSCORES")
		if err != nil {
			return typ, nil, err
		}
		s := strs(v)
		var out []any
		for i := 0; i+1 < len(s); i += 2 {
			out = append(out, []any{s[i], s[i+1]})
		}
		return typ, out, nil
	}
	return typ, nil, nil // streams and modules: type only
}

func anys(s []string) []any {
	out := make([]any, len(s))
	for i, x := range s {
		out[i] = x
	}
	return out
}

func readRedis(a []string, o readOpts) ([]Record, bool, error) {
	if len(a) < 2 {
		return nil, false, usagef("redis <addr> get|scan|hgetall|smembers|lrange|zrange|cmd ...")
	}
	addr, op, rest := a[0], a[1], a[2:]
	need := func(n int, form string) error {
		if len(rest) != n {
			return usagef("redis <addr> %s", form)
		}
		return nil
	}
	switch op {
	case "get":
		if len(rest) == 0 {
			return nil, false, usagef("redis <addr> get <key>...")
		}
	case "scan":
		if err := need(1, "scan <pattern>"); err != nil {
			return nil, false, err
		}
	case "hgetall", "smembers", "lrange", "zrange":
		if err := need(1, op+" <key>"); err != nil {
			return nil, false, err
		}
	case "cmd":
		if len(rest) == 0 {
			return nil, false, usagef("redis <addr> cmd <word>...")
		}
	default:
		return nil, false, usagef("redis: unknown operation %q", op)
	}
	rc, err := dialRedis(addr, o.timeout)
	if err != nil {
		return nil, false, err
	}
	defer rc.c.Close()
	fail := func(err error) ([]Record, bool, error) { return nil, false, cannot(err) }
	var out []Record
	switch op {
	case "get":
		for _, k := range rest {
			v, err := rc.do("GET", k)
			if err != nil {
				return fail(err)
			}
			out = append(out, Record{"key": k, "value": v})
		}
	case "scan":
		cursor := "0"
		seen := map[string]bool{}
		var keys []string
		for {
			v, err := rc.do("SCAN", cursor, "MATCH", rest[0], "COUNT", "1000")
			if err != nil {
				return fail(err)
			}
			arr, _ := v.([]any)
			if len(arr) != 2 {
				return fail(errors.New("redis: bad SCAN reply"))
			}
			cursor, _ = arr[0].(string)
			for _, k := range strs(arr[1]) {
				if !seen[k] {
					seen[k] = true
					keys = append(keys, k)
				}
			}
			if cursor == "0" {
				break
			}
		}
		sort.Strings(keys)
		for _, k := range keys {
			typ, v, err := rc.value(k)
			if err != nil {
				return fail(err)
			}
			if typ == "none" { // deleted between SCAN and TYPE
				continue
			}
			out = append(out, Record{"key": k, "type": typ, "value": v})
		}
	case "hgetall":
		v, err := rc.do("HGETALL", rest[0])
		if err != nil {
			return fail(err)
		}
		s := strs(v)
		for i := 0; i+1 < len(s); i += 2 {
			out = append(out, Record{"key": rest[0], "field": s[i], "value": s[i+1]})
		}
	case "smembers":
		v, err := rc.do("SMEMBERS", rest[0])
		if err != nil {
			return fail(err)
		}
		for _, m := range strs(v) {
			out = append(out, Record{"key": rest[0], "member": m})
		}
	case "lrange":
		v, err := rc.do("LRANGE", rest[0], "0", "-1")
		if err != nil {
			return fail(err)
		}
		for i, m := range strs(v) {
			out = append(out, Record{"key": rest[0], "index": int64(i), "value": m})
		}
		return out, true, nil
	case "zrange":
		v, err := rc.do("ZRANGE", rest[0], "0", "-1", "WITHSCORES")
		if err != nil {
			return fail(err)
		}
		s := strs(v)
		for i := 0; i+1 < len(s); i += 2 {
			out = append(out, Record{"key": rest[0], "member": s[i], "score": s[i+1]})
		}
		return out, true, nil
	case "cmd":
		v, err := rc.do(rest...)
		if err != nil {
			return fail(err)
		}
		out = append(out, Record{"reply": v})
	}
	return out, false, nil
}

// ---------------------------------------------------------------- etcd (v3 JSON gateway)

func etcdBase(ep string) string {
	if !strings.HasPrefix(ep, "http://") && !strings.HasPrefix(ep, "https://") {
		ep = "http://" + ep
	}
	return strings.TrimRight(ep, "/")
}

func etcdPost(ep, path string, body any, t time.Duration, out any) error {
	b, _ := json.Marshal(body)
	cl := &http.Client{Timeout: t}
	resp, err := cl.Post(etcdBase(ep)+path, "application/json", bytes.NewReader(b))
	if err != nil {
		return cannot(err)
	}
	defer resp.Body.Close()
	rb, err := io.ReadAll(resp.Body)
	if err != nil {
		return cannot(err)
	}
	if resp.StatusCode != 200 {
		return cannotf("etcd %s: HTTP %d: %s", path, resp.StatusCode, strings.TrimSpace(string(rb)))
	}
	if err := json.Unmarshal(rb, out); err != nil {
		return cannotf("etcd %s: %v", path, err)
	}
	return nil
}

// prefixEnd is clientv3.GetPrefixRangeEnd.
func prefixEnd(p string) string {
	b := []byte(p)
	for i := len(b) - 1; i >= 0; i-- {
		if b[i] < 0xff {
			b[i]++
			return string(b[:i+1])
		}
	}
	return "\x00"
}

func i64(s string) int64 { n, _ := strconv.ParseInt(s, 10, 64); return n }

func readEtcd(a []string, o readOpts) ([]Record, error) {
	if len(a) < 2 {
		return nil, usagef("etcd <endpoint> get <key> [--prefix] [--serializable] | status")
	}
	ep, op := a[0], a[1]
	b64 := base64.StdEncoding.EncodeToString
	switch op {
	case "get":
		var key string
		prefix, serial := false, false
		for _, x := range a[2:] {
			switch x {
			case "--prefix":
				prefix = true
			case "--serializable":
				serial = true
			default:
				if key != "" {
					return nil, usagef("etcd get: one key")
				}
				key = x
			}
		}
		req := map[string]any{"key": b64([]byte(key)), "serializable": serial}
		if prefix {
			if key == "" {
				req["key"] = b64([]byte{0})
			}
			req["range_end"] = b64([]byte(prefixEnd(key)))
		}
		var resp struct {
			Kvs []struct {
				Key, Value                           string
				CreateRevision string `json:"create_revision"`
				ModRevision    string `json:"mod_revision"`
				Version        string
			}
		}
		if err := etcdPost(ep, "/v3/kv/range", req, o.timeout, &resp); err != nil {
			return nil, err
		}
		var out []Record
		for _, kv := range resp.Kvs {
			k, _ := base64.StdEncoding.DecodeString(kv.Key)
			v, _ := base64.StdEncoding.DecodeString(kv.Value)
			out = append(out, Record{"key": bytesValue(k, o.base64), "value": bytesValue(v, o.base64),
				"create_revision": i64(kv.CreateRevision), "mod_revision": i64(kv.ModRevision), "version": i64(kv.Version)})
		}
		return out, nil
	case "status":
		if len(a) != 2 {
			return nil, usagef("etcd <endpoint> status")
		}
		var resp struct {
			Header struct {
				Revision string
				RaftTerm string `json:"raft_term"`
			}
			RaftIndex string `json:"raftIndex"`
			RaftTerm  string `json:"raftTerm"`
		}
		if err := etcdPost(ep, "/v3/maintenance/status", map[string]any{}, o.timeout, &resp); err != nil {
			return nil, err
		}
		return []Record{{"endpoint": ep, "revision": i64(resp.Header.Revision),
			"raft_index": i64(resp.RaftIndex), "raft_term": i64(resp.RaftTerm)}}, nil
	}
	return nil, usagef("etcd: unknown operation %q", op)
}
