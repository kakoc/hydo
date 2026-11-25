// kvc is the test client of the Kvrocks example: the cluster's setup, the workload's operations,
// the failover, the read-back and the final checks. It speaks RESP with go-redis, without its
// retries, so that one command is sent once.
//
//	kvc ready <addr>      PING answers PONG (a service's readiness check)
//	kvc init              wait until the master kv1 has both replicas in step (first_init)
//	kvc primary           print the current master (the group's primary: command)
//	kvc op <kind>         one write; the composer's HYDO_* variables drive it
//	kvc failover          promote a replica, as Redis Sentinel does (serial_driver_failover)
//	kvc flush             FLUSHMEMTABLE on the master (serial_driver_flush)
//	kvc read              reads of known keys through any node (anytime_read)
//	kvc readback          the final: of the messages check: each write's effect, read back
//	kvc check <rule>      one final check: converge, wait, atomic, counters
//	kvc dump <node>       print what one node holds of the test's keys (for a look by hand)
//
// The nodes are kv1, kv2 and kv3 on port 6666. At the start kv1 is the master and the others its
// replicas. The current master is in /data/kvc/master, a file on the data disk that the failover
// writes and syncs, so that a recovery boot after a power cut knows it too.
//
// The mode (/data/kvc/ack, the token M of hydo.yaml; wait without the file) says what acknowledges a write:
//
//	wait     the master's reply and then WAIT 2 5000 answering 2: both replicas have the write.
//	         A reply that WAIT does not confirm is an unknown outcome. The failover runs.
//	master   the master's reply alone. The failover does nothing, so a write must survive
//	         kills and restarts, and power cuts with rocksdb.write_options.sync yes.
//
// Writes, one id each: <kind>.<key>.<seq>.<value>. A register is one string key, or one field of
// a hash. A member is one element that the write adds to a list, a set, a sorted set, or to the
// keys of a transaction or a Lua script.
//
//	set    SET of a new string key                 setex  SET ... PXAT of a new key
//	over   SET or HSET over an existing register   cas    CAS (Kvrocks's compare-and-set)
//	del    DEL of an existing register's key       hset   HSET of a new hash's field
//	incr   INCR of one of four counters            rpush  RPUSH of a new element
//	sadd   SADD of a new member                    zadd   ZADD of a new member
//	txn    MULTI, SET, HSET, SADD, RPUSH, EXEC on four new keys
//	lua    EVAL of a script that SETs and SADDs two new keys
//
// The value of a write is the tag v<cmd>; a SET ... PXAT's tag carries its deadline in ms,
// v<cmd>t<deadline>, so that the read-back knows the deadline in a recovery boot too.
//
// One command at a time works on a register (a lock file under /tmp/kvc/lock). A register's last
// known value and seq are in /tmp/kvc/keys/<register>, for the registers whose every write got a
// certain answer. A register whose write got no certain answer is never written again, so each
// register ends with at most one write of unknown outcome.
//
// Outcomes. $HYDO_ACKED gets the id of each acknowledged write, $HYDO_OUTCOMES gets
// "<id> ack|failed|unknown". An error reply that means Kvrocks did nothing (READONLY, LOADING,
// EXECABORT, a CAS or DEL that found another value) is failed; every other error, a timeout and
// no answer are unknown.
//
// The history writes (hydo guide messages): each write is a send of its id to the channel
// writes. `kvc readback` reads every register and member back from the master and writes the
// final state of each write: s=ok for acknowledged, s=fail for failed, s=info for unknown, and
// f=1 or f=0 for whether the write's effect is there now, wherever that is decided.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math/rand/v2"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/redis/go-redis/v9"
)

const (
	port       = "6666"
	stateDir   = "/tmp/kvc"
	masterFile = "/data/kvc/master"
	waitMs     = 5000
)

var nodes = []string{"kv1", "kv2", "kv3"}

func envDef(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func atoiDef(s string, d int) int {
	if v, err := strconv.Atoi(s); err == nil {
		return v
	}
	return d
}

func cmdID() int { return atoiDef(os.Getenv("HYDO_CMD_ID"), 0) }

func rng() *rand.Rand {
	s := uint64(atoiDef(os.Getenv("HYDO_SEED"), 0))
	return rand.New(rand.NewPCG(s, uint64(cmdID())))
}

func mode() string {
	b, err := os.ReadFile("/data/kvc/ack")
	if err != nil {
		return "wait"
	}
	return strings.TrimSpace(string(b))
}

func appendLine(path, line string) {
	if path == "" {
		return
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	f.WriteString(line + "\n")
	f.Close()
}

func readLines(path string) []string {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []string
	for _, l := range strings.Split(string(b), "\n") {
		if l = strings.TrimSpace(l); l != "" {
			out = append(out, l)
		}
	}
	return out
}

// console writes one line to the guest's console, where reactions (A=) and the run's log see it.
func console(line string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	f.WriteString(line + "\n")
}

// event writes an SDK assertion record: the event of a reaction (A=kvrocks.write_replied:cut).
func event(id, msg string) {
	b, _ := json.Marshal(map[string]any{"t": "sometimes", "id": id, "ok": true, "msg": msg})
	console("HYDO-ASSERT " + string(b))
}

// uptimeNs is the guest's uptime in ns, as the history records write it.
func uptimeNs() string {
	b, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return "-"
	}
	f := strings.Fields(string(b))
	if len(f) == 0 {
		return "-"
	}
	return strings.Replace(f[0], ".", "", 1) + "0000000"
}

// hrec writes one record of the history writes: HYDO-HIST writes <process> <type> <time> <args>.
func hrec(proc int, typ string, args ...string) {
	out := envDef("HYDO_HIST_OUT", "/dev/console")
	if out == "off" {
		return
	}
	f, err := os.OpenFile(out, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	f.WriteString(fmt.Sprintf("HYDO-HIST writes %d %s %s %s\n", proc, typ, uptimeNs(), strings.Join(args, " ")))
}

// enc encodes a value of a history item: %, a space, and the characters the records use.
func enc(s string) string {
	var b strings.Builder
	for _, c := range []byte(s) {
		if c <= ' ' || strings.IndexByte("%@,[]:;=", c) >= 0 {
			fmt.Fprintf(&b, "%%%02X", c)
		} else {
			b.WriteByte(c)
		}
	}
	return b.String()
}

// ---- RESP ----

func client(node string, timeout time.Duration) *redis.Client {
	return redis.NewClient(&redis.Options{
		Addr:            net.JoinHostPort(node, port),
		DialTimeout:     3 * time.Second,
		ReadTimeout:     timeout,
		WriteTimeout:    timeout,
		MaxRetries:      -1, // one command is sent once
		PoolSize:        1,
		Protocol:        2,
		DisableIdentity: true,
	})
}

func ctxT(d time.Duration) (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), d)
}

// certain says whether an error reply means that Kvrocks did nothing.
func certain(err error) bool {
	var oe *net.OpError
	if errors.As(err, &oe) && oe.Op == "dial" {
		return true // the connection never opened: nothing was sent
	}
	var re redis.Error
	if !errors.As(err, &re) {
		return false
	}
	m := strings.ToUpper(err.Error())
	for _, p := range []string{"READONLY", "LOADING", "EXECABORT", "MASTERDOWN", "NOREPLICAS", "WRONGTYPE"} {
		if strings.HasPrefix(m, p) {
			return true
		}
	}
	return false
}

func errText(err error) string {
	if err == nil {
		return ""
	}
	s := err.Error()
	if len(s) > 200 {
		s = s[:200]
	}
	return s
}

// ---- the master ----

func currentMaster() string {
	b, err := os.ReadFile(masterFile)
	if err == nil {
		if m := strings.TrimSpace(string(b)); m != "" {
			return m
		}
	}
	return "kv1"
}

func setMaster(m string) {
	os.MkdirAll(filepath.Dir(masterFile), 0o755)
	tmp := masterFile + ".new"
	f, err := os.Create(tmp)
	if err != nil {
		return
	}
	f.WriteString(m + "\n")
	f.Sync()
	f.Close()
	os.Rename(tmp, masterFile)
	if d, err := os.Open(filepath.Dir(masterFile)); err == nil {
		d.Sync()
		d.Close()
	}
}

// role returns ROLE's first field and, for a replica, its master's host.
func role(node string) (string, string, error) {
	c := client(node, 3*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(4 * time.Second)
	defer cancel()
	v, err := c.Do(ctx, "ROLE").Slice()
	if err != nil {
		return "", "", err
	}
	if len(v) == 0 {
		return "", "", fmt.Errorf("empty ROLE")
	}
	r := fmt.Sprint(v[0])
	host := ""
	if r == "slave" && len(v) > 1 {
		host = fmt.Sprint(v[1])
	}
	return r, host, nil
}

// reconfigure makes node a replica of master (or the master itself) and persists the role
// with CONFIG REWRITE, as Sentinel does.
func reconfigure(node, master string) error {
	c := client(node, 5*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(40 * time.Second)
	defer cancel()
	var err error
	if node == master {
		err = c.Do(ctx, "SLAVEOF", "NO", "ONE").Err()
	} else {
		// An old master lists the promoted node among its replicas until a send to it fails,
		// and refuses SLAVEOF to it until then ("can't replicate your own replicas").
		for try := 0; ; try++ {
			err = c.Do(ctx, "SLAVEOF", master, port).Err()
			if err == nil || try >= 20 || !strings.Contains(err.Error(), "your own replicas") {
				break
			}
			time.Sleep(time.Second)
		}
	}
	if err != nil {
		return err
	}
	return c.Do(ctx, "CONFIG", "REWRITE").Err()
}

// reconcile brings every node that answers into the role that the master file gives it. A node
// that was down during a failover comes back with its old role.
func reconcile() {
	m := currentMaster()
	for _, n := range nodes {
		r, host, err := role(n)
		if err != nil {
			continue
		}
		if n == m && r != "master" || n != m && (r != "slave" || !sameNode(host, m)) {
			if err := reconfigure(n, m); err != nil {
				fmt.Fprintf(os.Stderr, "kvc: reconfigure %s under %s: %s\n", n, m, errText(err))
			} else {
				fmt.Fprintf(os.Stderr, "kvc: %s reconfigured under the master %s (it was %s %s)\n", n, m, r, host)
			}
		}
	}
}

var addrs = map[string]string{"kv1": "10.89.0.11", "kv2": "10.89.0.12", "kv3": "10.89.0.13"}

func sameNode(host, node string) bool { return host == node || host == addrs[node] }

// replInfo reads INFO replication into a map.
func replInfo(node string) (map[string]string, error) {
	c := client(node, 3*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(4 * time.Second)
	defer cancel()
	s, err := c.Info(ctx, "replication").Result()
	if err != nil {
		return nil, err
	}
	m := map[string]string{}
	for _, l := range strings.Split(s, "\n") {
		if k, v, ok := strings.Cut(strings.TrimSpace(l), ":"); ok {
			m[k] = v
		}
	}
	return m, nil
}

// inStep says whether the master has both replicas connected, and with needLag also whether it
// reports lag 0 for both (the lag counts from the replicas' acknowledgements).
func inStep(master string, needLag bool) (bool, string) {
	m, err := replInfo(master)
	if err != nil {
		return false, errText(err)
	}
	if m["role"] != "master" {
		return false, master + " is " + m["role"]
	}
	if atoiDef(m["connected_slaves"], 0) < 2 {
		return false, master + " has " + m["connected_slaves"] + " replicas"
	}
	for i := 0; i < 2 && needLag; i++ {
		s := m[fmt.Sprintf("slave%d", i)]
		if !strings.Contains(s, "lag=0") {
			return false, "slave" + strconv.Itoa(i) + " " + s
		}
	}
	return true, ""
}

// ---- state ----

type keyState struct {
	seq int
	val string
}

func lock(key string) bool {
	os.MkdirAll(filepath.Join(stateDir, "lock"), 0o755)
	f, err := os.OpenFile(filepath.Join(stateDir, "lock", key), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o644)
	if err != nil {
		return false
	}
	f.Close()
	return true
}

func unlock(key string) { os.Remove(filepath.Join(stateDir, "lock", key)) }

func loadState(key string) (keyState, bool) {
	b, err := os.ReadFile(filepath.Join(stateDir, "keys", key))
	if err != nil {
		return keyState{}, false
	}
	f := strings.Fields(string(b))
	if len(f) != 2 {
		return keyState{}, false
	}
	return keyState{atoiDef(f[0], 0), f[1]}, true
}

func saveState(key string, s keyState) {
	os.MkdirAll(filepath.Join(stateDir, "keys"), 0o755)
	tmp := filepath.Join(stateDir, "keys", "."+key)
	os.WriteFile(tmp, []byte(fmt.Sprintf("%d %s\n", s.seq, s.val)), 0o644)
	os.Rename(tmp, filepath.Join(stateDir, "keys", key))
}

func retire(key string) { os.Remove(filepath.Join(stateDir, "keys", key)) }

// pickKey locks a known register that exists now and whose name starts with one of prefixes.
func pickKey(r *rand.Rand, prefixes string) (string, keyState) {
	ents, _ := os.ReadDir(filepath.Join(stateDir, "keys"))
	var names []string
	for _, e := range ents {
		if n := e.Name(); !strings.HasPrefix(n, ".") && strings.ContainsRune(prefixes, rune(n[0])) {
			names = append(names, n)
		}
	}
	sort.Strings(names)
	r.Shuffle(len(names), func(i, j int) { names[i], names[j] = names[j], names[i] })
	for _, k := range names {
		if !lock(k) {
			continue
		}
		s, ok := loadState(k)
		if ok && s.val != "-" {
			return k, s
		}
		unlock(k)
	}
	return "", keyState{}
}

// ---- registers and members ----

// A register's name is its key (s<n>, e<n>) or h<n>_f for the field f of the hash h<n>.
func regKey(reg string) (key, field string) {
	if k, f, ok := strings.Cut(reg, "_"); ok {
		return k, f
	}
	return reg, ""
}

// deadline is the PXAT deadline in ms that a tag carries, or 0.
func deadline(tag string) int64 {
	if _, t, ok := strings.Cut(tag, "t"); ok {
		v, _ := strconv.ParseInt(t, 10, 64)
		return v
	}
	return 0
}

// readReg reads a register: its value, or "-" when it does not exist.
func readReg(c *redis.Client, reg string) (string, error) {
	ctx, cancel := ctxT(10 * time.Second)
	defer cancel()
	k, f := regKey(reg)
	var v string
	var err error
	if f != "" {
		v, err = c.HGet(ctx, k, f).Result()
	} else {
		v, err = c.Get(ctx, k).Result()
	}
	if err == redis.Nil {
		return "-", nil
	}
	return v, err
}

// A member write's key names its container: l<n> list, z<n> set, q<n> sorted set, t<n> and
// u<n> the keys of a transaction and of a script.
func hasMember(c *redis.Client, kind, key, tag string) (bool, string, error) {
	ctx, cancel := ctxT(10 * time.Second)
	defer cancel()
	switch kind {
	case "rpush":
		v, err := c.LRange(ctx, key, 0, -1).Result()
		if err != nil {
			return false, "", err
		}
		n := 0
		for _, e := range v {
			if e == tag {
				n++
			}
		}
		if n > 1 {
			return true, fmt.Sprintf("%s holds %s %d times", key, tag, n), nil
		}
		return n == 1, "", nil
	case "sadd":
		ok, err := c.SIsMember(ctx, key, tag).Result()
		return ok, "", err
	case "zadd":
		s, err := c.ZScore(ctx, key, tag).Result()
		if err == redis.Nil {
			return false, "", nil
		}
		if err != nil {
			return false, "", err
		}
		if want := float64(atoiDef(strings.TrimPrefix(tag, "v"), -1)); s != want {
			return true, fmt.Sprintf("%s holds %s with the score %v, not %v", key, tag, s, want), nil
		}
		return true, "", nil
	case "txn", "lua":
		parts, err := groupParts(c, kind, key, tag)
		if err != nil {
			return false, "", err
		}
		n := 0
		for _, p := range parts {
			if p {
				n++
			}
		}
		if n != 0 && n != len(parts) {
			return false, fmt.Sprintf("%s %s has %d of its %d writes", kind, key, n, len(parts)), nil
		}
		return n == len(parts), "", nil
	}
	return false, "", fmt.Errorf("unknown member kind %s", kind)
}

// groupParts says which of a transaction's or a script's writes are there.
func groupParts(c *redis.Client, kind, key, tag string) ([]bool, error) {
	ctx, cancel := ctxT(10 * time.Second)
	defer cancel()
	var out []bool
	eq := func(v string, err error) error {
		if err == redis.Nil {
			out = append(out, false)
			return nil
		}
		if err != nil {
			return err
		}
		out = append(out, v == tag)
		return nil
	}
	if err := eq(c.Get(ctx, key+"a").Result()); err != nil {
		return nil, err
	}
	if kind == "txn" {
		if err := eq(c.HGet(ctx, key+"b", "f").Result()); err != nil {
			return nil, err
		}
	}
	ok, err := c.SIsMember(ctx, key+"c", tag).Result()
	if err != nil {
		return nil, err
	}
	out = append(out, ok)
	if kind == "txn" {
		v, err := c.LRange(ctx, key+"d", 0, -1).Result()
		if err != nil {
			return nil, err
		}
		out = append(out, len(v) == 1 && v[0] == tag)
	}
	return out, nil
}

// ---- one write ----

type op struct {
	kind, key string
	seq       int
	val       string
}

func (o op) id() string { return fmt.Sprintf("%s.%s.%d.%s", o.kind, o.key, o.seq, o.val) }

func parseID(id string) (op, bool) {
	f := strings.Split(id, ".")
	if len(f) != 4 {
		return op{}, false
	}
	return op{f[0], f[1], atoiDef(f[2], -1), f[3]}, true
}

func isRegister(kind string) bool {
	switch kind {
	case "set", "setex", "over", "cas", "del", "hset", "late":
		return true
	}
	return false
}

func begin(o op) {
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" unknown")
	hrec(cmdID(), "invoke", "send", "writes", o.id())
}

// finish records the outcome of o and updates the register's state. It returns the outcome.
func finish(o op, oc string, err error, before keyState, existed bool) string {
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" "+oc)
	reg := isRegister(o.kind)
	switch oc {
	case "ack":
		appendLine(os.Getenv("HYDO_ACKED"), o.id())
		hrec(cmdID(), "ok", "send", "writes", o.id())
		if reg {
			saveState(o.key, keyState{o.seq, o.val})
		}
		event("kvrocks.write_acked", "Kvrocks acknowledged a "+o.kind)
	case "failed":
		hrec(cmdID(), "fail", "send", "writes", o.id())
		if reg && existed {
			saveState(o.key, keyState{o.seq, before.val})
		}
		fmt.Fprintf(os.Stderr, "kvc: %s: failed: %s\n", o.id(), errText(err))
	default:
		hrec(cmdID(), "info", "send", "writes", o.id())
		if reg {
			retire(o.key)
		}
		fmt.Fprintf(os.Stderr, "kvc: %s: unknown: %s\n", o.id(), errText(err))
	}
	return oc
}

// confirm turns the master's reply into an outcome: in the mode wait, WAIT 2 must answer 2.
func confirm(c *redis.Client, err error) (string, error) {
	if err != nil {
		if certain(err) {
			return "failed", err
		}
		return "unknown", err
	}
	event("kvrocks.write_replied", "the master replied to a write")
	if mode() != "wait" {
		return "ack", nil
	}
	ctx, cancel := ctxT(waitMs*time.Millisecond + 10*time.Second)
	defer cancel()
	n, werr := c.Do(ctx, "WAIT", 2, waitMs).Int()
	if werr != nil {
		return "unknown", fmt.Errorf("WAIT: %w", werr)
	}
	if n < 2 {
		return "unknown", fmt.Errorf("WAIT 2 %d answered %d", waitMs, n)
	}
	return "ack", nil
}

func runOp(kind string) int {
	seeClock()
	defer seeClock()
	r := rng()
	cmd := cmdID()
	tag := fmt.Sprintf("v%d", cmd)
	master := currentMaster()
	c := client(master, 15*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(20 * time.Second)
	defer cancel()

	var o op
	var before keyState
	existed := false
	var err error
	failed := "" // a reply that means Kvrocks did nothing, and why

	switch kind {
	case "set", "setex", "hset":
		key := fmt.Sprintf("s%d", cmd)
		switch kind {
		case "setex":
			key = fmt.Sprintf("e%d", cmd)
			ttl := []int64{3, 8, 20, 60, 3600}[r.IntN(5)]
			tag = fmt.Sprintf("v%dt%d", cmd, time.Now().UnixMilli()+ttl*1000)
		case "hset":
			key = fmt.Sprintf("h%d_f", cmd)
		}
		lock(key)
		defer unlock(key)
		o = op{kind, key, 0, tag}
		begin(o)
		k, f := regKey(key)
		switch kind {
		case "set":
			err = c.Set(ctx, k, tag, 0).Err()
		case "setex":
			err = c.Do(ctx, "SET", k, tag, "PXAT", deadline(tag)).Err()
		case "hset":
			err = c.HSet(ctx, k, f, tag).Err()
		}
	case "over", "cas", "del":
		prefixes := "sh"
		if kind != "over" {
			prefixes = "s"
		}
		key, s := pickKey(r, prefixes)
		if key == "" {
			return runOp("set")
		}
		defer unlock(key)
		before, existed = s, true
		val := tag
		if kind == "del" {
			val = "-"
		}
		o = op{kind, key, s.seq + 1, val}
		begin(o)
		k, f := regKey(key)
		switch kind {
		case "over":
			if f != "" {
				err = c.HSet(ctx, k, f, tag).Err()
			} else {
				err = c.Set(ctx, k, tag, 0).Err()
			}
		case "cas":
			var n int64
			n, err = c.Do(ctx, "CAS", k, s.val, tag).Int64()
			if err == nil && n != 1 {
				failed = fmt.Sprintf("CAS %s %s answered %d", k, s.val, n)
			}
		case "del":
			var n int64
			n, err = c.Del(ctx, k).Result()
			if err == nil && n != 1 {
				failed = fmt.Sprintf("DEL %s answered %d", k, n)
			}
		}
	case "incr":
		key := fmt.Sprintf("c%d", r.IntN(4))
		o = op{kind, key, cmd, "+1"}
		begin(o)
		err = c.Incr(ctx, key).Err()
	case "rpush", "sadd", "zadd":
		pre := map[string]string{"rpush": "l", "sadd": "z", "zadd": "q"}[kind]
		key := fmt.Sprintf("%s%d", pre, r.IntN(4))
		o = op{kind, key, 0, tag}
		begin(o)
		switch kind {
		case "rpush":
			err = c.RPush(ctx, key, tag).Err()
		case "sadd":
			var n int64
			n, err = c.SAdd(ctx, key, tag).Result()
			if err == nil && n != 1 {
				failed = fmt.Sprintf("SADD %s %s answered %d", key, tag, n)
			}
		case "zadd":
			err = c.ZAdd(ctx, key, redis.Z{Score: float64(cmd), Member: tag}).Err()
		}
	case "txn":
		key := fmt.Sprintf("t%d", cmd)
		o = op{kind, key, 0, tag}
		begin(o)
		_, err = c.TxPipelined(ctx, func(p redis.Pipeliner) error {
			p.Set(ctx, key+"a", tag, 0)
			p.HSet(ctx, key+"b", "f", tag)
			p.SAdd(ctx, key+"c", tag)
			p.RPush(ctx, key+"d", tag)
			return nil
		})
	case "lua":
		key := fmt.Sprintf("u%d", cmd)
		o = op{kind, key, 0, tag}
		begin(o)
		err = c.Eval(ctx, "redis.call('SET', KEYS[1], ARGV[1]); redis.call('SADD', KEYS[2], ARGV[1]); return 1",
			[]string{key + "a", key + "c"}, tag).Err()
	default:
		fmt.Fprintln(os.Stderr, "kvc: unknown operation", kind)
		return 2
	}

	if failed != "" {
		finish(o, "failed", errors.New(failed), before, existed)
		// the register's last value is known, so the reply contradicts what was acknowledged
		if (kind == "cas" || kind == "del") && mode() == "master" {
			v := fmt.Sprintf("read-your-writes: %s: %s, but %s's last acknowledged value is %s", o.id(), failed, o.key, before.val)
			console("kvc: " + v)
			fmt.Println(v)
			return 100
		}
		return 1
	}
	oc, cerr := confirm(c, err)
	if finish(o, oc, cerr, before, existed) != "ack" {
		return 1
	}
	if mode() == "wait" {
		if v := readAfterWait(o, master); v != "" {
			console("kvc: " + v)
			fmt.Println(v)
			return 100
		}
	}
	return 0
}

// readAfterWait reads an acknowledged write from both replicas: WAIT 2 said that both have it.
func readAfterWait(o op, master string) string {
	for _, n := range nodes {
		if n == master {
			continue
		}
		c := client(n, 5*time.Second)
		var got string
		var err error
		if isRegister(o.kind) {
			var v string
			v, err = readReg(c, o.key)
			if err == nil && v != o.val && !(o.kind == "setex" && v == "-" && time.Now().UnixMilli() > deadline(o.val)-1000) {
				got = "holds " + v
			}
		} else if o.kind != "incr" {
			var ok bool
			var odd string
			ok, odd, err = hasMember(c, o.kind, o.key, o.val)
			if err == nil && (!ok || odd != "") {
				got = "lacks it" + odd
			}
		}
		c.Close()
		if err != nil {
			continue // the replica went away: no certain answer
		}
		if got != "" {
			return fmt.Sprintf("read-after-wait: %s was acknowledged by WAIT 2 on the master %s, and then the replica %s %s", o.id(), master, n, got)
		}
	}
	return ""
}

// ---- the failover ----

// failover promotes a replica of the current master, then points the other nodes at it and
// persists every role with CONFIG REWRITE. It runs in one of two calls, by the seed.
func failover() int {
	r := rng()
	if mode() != "wait" || r.IntN(2) != 0 {
		return 0
	}
	reconcile()
	old := currentMaster()
	var cands []string
	for _, n := range nodes {
		if n != old {
			cands = append(cands, n)
		}
	}
	nm := cands[r.IntN(len(cands))]
	if rl, host, err := role(nm); err != nil || rl != "slave" || !sameNode(host, old) {
		fmt.Printf("kvc: failover: %s is not a replica of %s now (%s %s %v): no failover\n", nm, old, rl, host, err)
		return 0
	}
	if err := reconfigure(nm, nm); err != nil {
		fmt.Printf("kvc: failover: SLAVEOF NO ONE on %s: %s\n", nm, errText(err))
		return 1
	}
	setMaster(nm)
	event("kvrocks.failover_promoted", "a replica was promoted")
	lateWrite(old)
	for _, n := range nodes {
		if n != nm {
			if err := reconfigure(n, nm); err != nil {
				fmt.Printf("kvc: failover: %s under %s: %s (reconciled later)\n", n, nm, errText(err))
			}
		}
	}
	event("kvrocks.failover_done", "every node that answers follows the new master")
	console(fmt.Sprintf("kvc: failover from %s to %s", old, nm))
	fmt.Printf("kvc: failover from %s to %s\n", old, nm)
	return 0
}

// lateWrite is the write of a client that has not learned of the failover yet: a SET to the old
// master after the promotion. WAIT 2 cannot confirm it, so its outcome is unknown at best. Its
// effect makes the old master's history differ from the new master's, so the old master rejoins
// with a full synchronization.
func lateWrite(old string) {
	key := fmt.Sprintf("s%dx", cmdID())
	o := op{"late", key, 0, fmt.Sprintf("v%dx", cmdID())}
	lock(key)
	defer unlock(key)
	begin(o)
	c := client(old, 5*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(8 * time.Second)
	defer cancel()
	err := c.Set(ctx, key, o.val, 0).Err()
	oc := "unknown"
	if err != nil && certain(err) {
		oc = "failed"
	}
	finish(o, oc, err, keyState{}, false)
}

// flushMemtable writes the master's memtables to SST files (FLUSHMEMTABLE), as a larger data set
// does by itself: the data of this test fits in the memtables, so without it a full
// synchronization would carry a WAL and no SST file.
func flushMemtable() int {
	c := client(currentMaster(), 15*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(20 * time.Second)
	defer cancel()
	if err := c.Do(ctx, "FLUSHMEMTABLE").Err(); err != nil {
		fmt.Fprintf(os.Stderr, "kvc: FLUSHMEMTABLE: %s\n", errText(err))
		return 1
	}
	event("kvrocks.memtable_flushed", "the master wrote its memtables to SST files")
	return 0
}

// ---- reads during the run ----

// written collects every value that a write of the run sent to each register and container,
// from $HYDO_OUTCOMES.
func written() map[string]map[string]bool {
	w := map[string]map[string]bool{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if len(f) != 2 {
			continue
		}
		o, ok := parseID(f[0])
		if !ok {
			continue
		}
		if w[o.key] == nil {
			w[o.key] = map[string]bool{"-": true}
		}
		w[o.key][o.val] = true
	}
	return w
}

// readSome reads up to five registers and containers through one node, any role. Each value must
// be one that some write sent: a replica never serves a value that no master had.
func readSome() int {
	r := rng()
	w := written()
	var keys []string
	for k := range w {
		if k[0] != 'c' && k[0] != 't' && k[0] != 'u' {
			keys = append(keys, k)
		}
	}
	sort.Strings(keys)
	r.Shuffle(len(keys), func(i, j int) { keys[i], keys[j] = keys[j], keys[i] })
	if len(keys) > 5 {
		keys = keys[:5]
	}
	n := nodes[r.IntN(len(nodes))]
	c := client(n, 5*time.Second)
	defer c.Close()
	for _, k := range keys {
		var vals []string
		switch k[0] {
		case 's', 'e', 'h':
			v, err := readReg(c, k)
			if err != nil {
				return 0
			}
			vals = []string{v}
		default:
			ctx, cancel := ctxT(5 * time.Second)
			var err error
			switch k[0] {
			case 'l':
				vals, err = c.LRange(ctx, k, 0, -1).Result()
			case 'z':
				vals, err = c.SMembers(ctx, k).Result()
			case 'q':
				vals, err = c.ZRange(ctx, k, 0, -1).Result()
			}
			cancel()
			if err != nil {
				return 0
			}
		}
		for _, v := range vals {
			if !w[k][v] {
				// a write may have started after this command read the outcomes
				if w2 := written(); w2[k][v] {
					continue
				}
				msg := fmt.Sprintf("phantom read: %s answered %s for %s, which no write sent", n, v, k)
				console("kvc: " + msg)
				fmt.Println(msg)
				return 100
			}
		}
	}
	return 0
}

// ---- the read-back ----

// opsByKey collects every write of the run from $HYDO_OUTCOMES (the last outcome of each id) and
// $HYDO_ACKED, grouped by key and sorted by seq.
func opsByKey() (map[string][]op, map[string]string) {
	oc := map[string]string{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if len(f) == 2 {
			oc[f[0]] = f[1]
		}
	}
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		oc[l] = "ack"
	}
	by := map[string][]op{}
	for id := range oc {
		if o, ok := parseID(id); ok {
			by[o.key] = append(by[o.key], o)
		}
	}
	for k := range by {
		sort.Slice(by[k], func(i, j int) bool { return by[k][i].seq < by[k][j].seq })
	}
	return by, oc
}

// expect is what a register's acknowledged value looks like now: the value, or "-" after a
// PXAT deadline, or "?" within a second of the deadline. A key whose deadline passed before a
// backward clock step expired as set, and Kvrocks may have removed it, so the value is required
// only while the latest clock that the run has seen (maxClock) is before the deadline too.
func expect(val string, now int64) string {
	d := deadline(val)
	switch {
	case d == 0:
		return val
	case now < d-1000 && maxClock() < d-1000:
		return val
	case now > d+1000:
		return "-"
	}
	return "?"
}

const maxClockFile = "/data/kvc/maxclock"

// seeClock records the wall clock in ms if it is later than every clock recorded before.
func seeClock() {
	now := time.Now().UnixMilli()
	if now <= maxClock() {
		return
	}
	os.MkdirAll(filepath.Dir(maxClockFile), 0o755)
	tmp := fmt.Sprintf("%s.%d", maxClockFile, os.Getpid())
	if os.WriteFile(tmp, []byte(strconv.FormatInt(now, 10)), 0o644) == nil {
		os.Rename(tmp, maxClockFile)
	}
}

// maxClock is the latest wall clock in ms that a command of the run recorded.
func maxClock() int64 {
	b, err := os.ReadFile(maxClockFile)
	if err != nil {
		return 0
	}
	v, _ := strconv.ParseInt(strings.TrimSpace(string(b)), 10, 64)
	return v
}

// waitMaster waits until the master answers, up to the deadline, after reconcile.
func waitMaster(until time.Time) (*redis.Client, string) {
	for {
		reconcile()
		m := currentMaster()
		c := client(m, 10*time.Second)
		ctx, cancel := ctxT(5 * time.Second)
		err := c.Ping(ctx).Err()
		cancel()
		if err == nil {
			return c, m
		}
		c.Close()
		if time.Now().After(until) {
			return nil, m
		}
		time.Sleep(2 * time.Second)
	}
}

// readBack is the final: of the messages check writes.
func readBack() int {
	const proc = 100000
	seeClock()
	by, oc := opsByKey()
	c, m := waitMaster(time.Now().Add(120 * time.Second))
	if c == nil {
		fmt.Printf("the master %s does not answer\n", m)
		return 0
	}
	defer c.Close()
	var keys []string
	for k := range by {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var ids, items, unread []string
	for _, k := range keys {
		ops := by[k]
		if isRegister(ops[0].kind) {
			v, err := readReg(c, k)
			if err != nil {
				for _, o := range ops {
					unread = append(unread, o.id())
				}
				continue
			}
			now := time.Now().UnixMilli()
			lastAck := -1
			for i, o := range ops {
				if oc[o.id()] == "ack" {
					lastAck = i
				}
			}
			cand := map[string]bool{}
			if lastAck >= 0 {
				cand[expect(ops[lastAck].val, now)] = true
			} else {
				cand["-"] = true
			}
			for i, o := range ops {
				if i > lastAck && oc[o.id()] == "unknown" {
					cand[expect(o.val, now)] = true
				}
			}
			for i, o := range ops {
				it := o.id()
				e := expect(o.val, now)
				switch oc[o.id()] {
				case "ack":
					it += ";s=ok"
					if i == lastAck && e != "?" && !cand["?"] {
						if v == e {
							it += ";f=1"
						} else if !cand[v] {
							it += ";f=0;e=" + enc("the key holds "+v)
						}
					}
				case "failed":
					it += ";s=fail;e=refused"
					if v == o.val && o.val != "-" && !cand[v] {
						it += ";f=1"
					}
				default:
					it += ";s=info"
				}
				ids, items = append(ids, o.id()), append(items, it)
			}
			if !cand[v] && !cand["?"] {
				var cs []string
				for x := range cand {
					cs = append(cs, x)
				}
				sort.Strings(cs)
				fmt.Printf("%s holds %s, which no acknowledged or unknown write left there (expected %s)\n", k, v, strings.Join(cs, " or "))
			}
			continue
		}
		for _, o := range ops {
			it := o.id()
			if o.kind == "incr" {
				switch oc[o.id()] {
				case "ack":
					it += ";s=ok"
				case "failed":
					it += ";s=fail;e=refused"
				default:
					it += ";s=info"
				}
				ids, items = append(ids, o.id()), append(items, it)
				continue
			}
			has, odd, err := hasMember(c, o.kind, o.key, o.val)
			if err != nil {
				unread = append(unread, o.id())
				continue
			}
			if odd != "" {
				fmt.Println(odd)
			}
			f := ";f=0"
			if has {
				f = ";f=1"
			}
			switch oc[o.id()] {
			case "ack":
				it += ";s=ok" + f
				if !has {
					it += ";e=" + enc("not there")
				}
			case "failed":
				it += ";s=fail;e=refused"
				if has {
					it += f
				}
			default:
				it += ";s=info" + f
			}
			ids, items = append(ids, o.id()), append(items, it)
		}
	}
	for i := 0; i < len(items); i += 20 {
		j := min(i+20, len(items))
		hrec(proc, "invoke", append([]string{"status", "writes"}, ids[i:j]...)...)
		hrec(proc, "ok", append([]string{"status", "writes"}, items[i:j]...)...)
	}
	for i := 0; i < len(unread); i += 20 {
		j := min(i+20, len(unread))
		hrec(proc, "invoke", append([]string{"status", "writes"}, unread[i:j]...)...)
		hrec(proc, "info", append([]string{"status", "writes"}, unread[i:j]...)...)
	}
	fmt.Printf("read back %d writes of %d keys from the master %s\n", len(items), len(keys), m)
	return 0
}

// ---- final checks ----

// dumpKey is a key's type and contents, in one string.
func dumpKey(c *redis.Client, k string) (string, error) {
	ctx, cancel := ctxT(10 * time.Second)
	defer cancel()
	t, err := c.Type(ctx, k).Result()
	if err != nil {
		return "", err
	}
	var v any
	switch t {
	case "none":
		return "-", nil
	case "string":
		v, err = c.Get(ctx, k).Result()
	case "hash":
		var m map[string]string
		m, err = c.HGetAll(ctx, k).Result()
		var ks []string
		for x, y := range m {
			ks = append(ks, x+"="+y)
		}
		sort.Strings(ks)
		v = ks
	case "list":
		v, err = c.LRange(ctx, k, 0, -1).Result()
	case "set":
		var s []string
		s, err = c.SMembers(ctx, k).Result()
		sort.Strings(s)
		v = s
	case "zset":
		v, err = c.ZRangeWithScores(ctx, k, 0, -1).Result()
	}
	if err == redis.Nil {
		return "-", nil
	}
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("%s %v", t, v), nil
}

// allKeys lists every key of a node with SCAN.
func allKeys(c *redis.Client) ([]string, error) {
	ctx, cancel := ctxT(30 * time.Second)
	defer cancel()
	var out []string
	var cur uint64
	for {
		ks, next, err := c.Scan(ctx, cur, "*", 500).Result()
		if err != nil {
			return nil, err
		}
		out = append(out, ks...)
		if next == 0 {
			break
		}
		cur = next
	}
	sort.Strings(out)
	return out, nil
}

// nearDeadline says whether a key is a PXAT key within 3 s of its deadline, whose presence
// two reads a moment apart may see differently.
func nearDeadline(c *redis.Client, k string, now int64) bool {
	if k[0] != 'e' {
		return false
	}
	v, err := readReg(c, k)
	if err != nil || v == "-" {
		return false
	}
	d := deadline(v)
	return d != 0 && now > d-3000 && now < d+3000
}

// checkConverge: after the heal every replica holds what the master holds, key for key.
func checkConverge() int {
	c, m := waitMaster(time.Now().Add(20 * time.Second))
	if c == nil {
		fmt.Printf("the master %s does not answer\n", m)
		return 2
	}
	defer c.Close()
	if ok, why := inStep(m, false); !ok {
		fmt.Printf("the replicas are not connected to the master %s: %s\n", m, why)
		return 1
	}
	mk, err := allKeys(c)
	if err != nil {
		fmt.Printf("SCAN on the master %s: %s\n", m, errText(err))
		return 2
	}
	now := time.Now().UnixMilli()
	want := map[string]string{}
	for _, k := range mk {
		if nearDeadline(c, k, now) {
			continue
		}
		d, err := dumpKey(c, k)
		if err != nil {
			return 2
		}
		want[k] = d
	}
	bad := 0
	for _, n := range nodes {
		if n == m {
			continue
		}
		rc := client(n, 10*time.Second)
		rk, err := allKeys(rc)
		if err != nil {
			rc.Close()
			fmt.Printf("SCAN on %s: %s\n", n, errText(err))
			return 2
		}
		seen := map[string]bool{}
		for _, k := range rk {
			seen[k] = true
			if nearDeadline(rc, k, now) {
				continue
			}
			d, err := dumpKey(rc, k)
			if err != nil {
				rc.Close()
				return 2
			}
			if w, ok := want[k]; !ok && d != "-" {
				if bad++; bad <= 5 {
					fmt.Printf("%s holds %s = %s, which the master %s does not hold\n", n, k, d, m)
				}
			} else if ok && w != d {
				if bad++; bad <= 5 {
					fmt.Printf("%s holds %s = %s, and the master %s holds %s\n", n, k, d, m, w)
				}
			}
		}
		for k, w := range want {
			if !seen[k] {
				if bad++; bad <= 5 {
					fmt.Printf("%s lacks %s, which the master %s holds as %s\n", n, k, m, w)
				}
			}
		}
		rc.Close()
	}
	if bad > 0 {
		fmt.Printf("a replica differs from the master %s in %d keys\n", m, bad)
		return 1
	}
	return 0
}

// checkWait: after the heal, with both replicas connected and holding what the master holds
// (eventually_converge), WAIT 2 on the master answers 2. It prints the master's view of each
// replica, whose offset counts from the replica's last acknowledgement.
func checkWait() int {
	c, m := waitMaster(time.Now().Add(20 * time.Second))
	if c == nil {
		fmt.Printf("the master %s does not answer\n", m)
		return 2
	}
	defer c.Close()
	if ok, why := inStep(m, false); !ok {
		fmt.Printf("the replicas are not connected to the master %s: %s\n", m, why)
		return 1
	}
	ctx, cancel := ctxT(waitMs*time.Millisecond + 10*time.Second)
	defer cancel()
	n, err := c.Do(ctx, "WAIT", 2, waitMs).Int()
	if err != nil {
		fmt.Printf("WAIT 2 %d on the master %s: %s\n", waitMs, m, errText(err))
		return 2
	}
	if n < 2 {
		info, _ := replInfo(m)
		fmt.Printf("WAIT 2 %d on the master %s answered %d; it reports slave0 %s and slave1 %s\n", waitMs, m, n, info["slave0"], info["slave1"])
		return 1
	}
	return 0
}

// checkAtomic: on every node each transaction and script has all of its writes or none.
func checkAtomic() int {
	by, _ := opsByKey()
	bad := 0
	for _, n := range nodes {
		c := client(n, 10*time.Second)
		for _, ops := range by {
			for _, o := range ops {
				if o.kind != "txn" && o.kind != "lua" {
					continue
				}
				_, odd, err := hasMember(c, o.kind, o.key, o.val)
				if err != nil {
					break
				}
				if odd != "" {
					if bad++; bad <= 5 {
						fmt.Printf("%s: %s\n", n, odd)
					}
				}
			}
		}
		c.Close()
	}
	if bad > 0 {
		fmt.Printf("%d transactions or scripts were applied in part\n", bad)
		return 1
	}
	return 0
}

// checkCounters: each counter on the master is at least its acknowledged INCRs and at most those
// and the unknown ones.
func checkCounters() int {
	by, oc := opsByKey()
	c, m := waitMaster(time.Now().Add(20 * time.Second))
	if c == nil {
		return 2
	}
	defer c.Close()
	bad := 0
	for k, ops := range by {
		if ops[0].kind != "incr" {
			continue
		}
		acked, unknown := 0, 0
		for _, o := range ops {
			switch oc[o.id()] {
			case "ack":
				acked++
			case "unknown":
				unknown++
			}
		}
		ctx, cancel := ctxT(5 * time.Second)
		v, err := c.Get(ctx, k).Int()
		cancel()
		if err == redis.Nil {
			v, err = 0, nil
		}
		if err != nil {
			return 2
		}
		if v < acked || v > acked+unknown {
			bad++
			fmt.Printf("%s on the master %s is %d, with %d acknowledged and %d unknown INCRs\n", k, m, v, acked, unknown)
		}
	}
	if bad > 0 {
		return 1
	}
	return 0
}

func cmdDump(node string) int {
	c := client(node, 10*time.Second)
	defer c.Close()
	ks, err := allKeys(c)
	if err != nil {
		fmt.Println(errText(err))
		return 2
	}
	for _, k := range ks {
		d, _ := dumpKey(c, k)
		fmt.Printf("%s %s\n", k, d)
	}
	return 0
}

// ---- setup ----

func cmdReady(addr string) int {
	c := client(addr, 3*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(3 * time.Second)
	defer cancel()
	if v, err := c.Ping(ctx).Result(); err != nil || v != "PONG" {
		return 1
	}
	return 0
}

func cmdInit() int {
	if _, err := os.Stat(masterFile); err != nil {
		setMaster("kv1")
	}
	until := time.Now().Add(150 * time.Second)
	for {
		reconcile()
		ok, why := inStep(currentMaster(), true)
		if ok {
			fmt.Printf("kvc: %s has both replicas in step\n", currentMaster())
			return 0
		}
		if time.Now().After(until) {
			fmt.Printf("kvc: the replicas are not in step after 150 s: %s\n", why)
			return 1
		}
		time.Sleep(2 * time.Second)
	}
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: kvc ready <addr> | init | primary | op <kind> | failover | read | readback | check <rule> | dump <node>")
		os.Exit(2)
	}
	arg := ""
	if len(os.Args) > 2 {
		arg = os.Args[2]
	}
	switch os.Args[1] {
	case "ready":
		os.Exit(cmdReady(arg))
	case "init":
		os.Exit(cmdInit())
	case "primary":
		fmt.Println(currentMaster())
		os.Exit(0)
	case "op":
		os.Exit(runOp(arg))
	case "failover":
		os.Exit(failover())
	case "flush":
		os.Exit(flushMemtable())
	case "read":
		os.Exit(readSome())
	case "readback":
		os.Exit(readBack())
	case "check":
		switch arg {
		case "converge":
			os.Exit(checkConverge())
		case "atomic":
			os.Exit(checkAtomic())
		case "wait":
			os.Exit(checkWait())
		case "counters":
			os.Exit(checkCounters())
		}
	case "dump":
		os.Exit(cmdDump(arg))
	}
	fmt.Fprintln(os.Stderr, "kvc: unknown command", os.Args[1:])
	os.Exit(2)
}
