// gnc is the test client of the Garnet example: the cluster's setup, the workload's operations,
// the failover, the slot migration, the checkpoints, the read-back and the final checks. It speaks
// RESP with go-redis, without its retries, so that one command is sent once. It follows a MOVED
// or an ASK reply itself: such a reply means that the node did nothing.
//
//	gnc ready <addr>        PING answers PONG (a service's readiness check)
//	gnc init                form the cluster: three primaries with one replica each (first_init)
//	gnc primary <shard>     print the primary of shard 1, 2 or 3 (the groups' primary: command)
//	gnc op <kind>           one write; the composer's HYDO_* variables drive it
//	gnc failover            CLUSTER FAILOVER on the replica of one shard (serial_driver_failover)
//	gnc migrate             MIGRATE ... SLOTS of one slot to another primary (serial_driver_migrate)
//	gnc save                BGSAVE on one primary (serial_driver_save)
//	gnc read                reads of known keys through any node (anytime_read)
//	gnc readback            the final: of the messages check: each write's effect, read back
//	gnc check <rule>        one final check: converge, slots, atomic, counters
//	gnc dump <node>         print what one node holds (for a look by hand)
//
// The nodes are g1 to g6 on port 6379. Shard i (1 to 3) is the pair gi and g(i+3); at the start gi
// is its primary. A failover swaps the roles inside a shard, and a migration moves a slot from one
// shard to another, so every command asks the cluster for the slots' owners.
//
// Keys. Every key carries one of 16 hash tags, {t0} to {t15}, so that the writes fall into 16
// slots and a migration moves keys. The key of the name s123 is {t11}s123 (123 mod 16 = 11).
//
// Writes, one id each: <kind>.<name>.<seq>.<value>. A register is one string key, or one field of
// a hash. A member is one element that the write adds to a list, a set, a sorted set, or to the
// keys of a transaction or a Lua script.
//
//	set    SET of a new string key                 setex  SET ... PXAT of a new key
//	over   SET or HSET over an existing register   del    DEL of an existing register's key
//	hset   HSET of a new hash's field              incr   INCR of one of 16 counters
//	rpush  RPUSH of a new element                  sadd   SADD of a new member
//	zadd   ZADD of a new member                    txn    MULTI, SET, HSET, SADD, RPUSH, EXEC
//	lua    EVAL of a script that SETs and SADDs two new keys
//	late   a SET to the old primary of a shard right after its failover
//
// The value of a write is the tag v<cmd>; a SET ... PXAT's tag carries its deadline in ms,
// v<cmd>t<deadline>, so that the read-back knows the deadline in a recovery boot too.
//
// One command at a time works on a register (a lock file under /tmp/gnc/lock). A register's last
// known value and seq are in /tmp/gnc/keys/<name>, for the registers whose every write got a
// certain answer. A register whose write got no certain answer is never written again, so each
// register ends with at most one write of unknown outcome.
//
// Outcomes. A write is acknowledged when the primary that owns its slot replied OK. $HYDO_ACKED
// gets the id of each acknowledged write, $HYDO_OUTCOMES gets "<id> ack|failed|unknown". An error
// reply that means Garnet did nothing (a redirect that was not followed, CLUSTERDOWN, TRYAGAIN,
// EXECABORT, a DEL that found nothing) is failed; every other error, a timeout and no answer are
// unknown.
//
// The history writes (hydo guide messages): each write is a send of its id to the channel
// writes. `gnc readback` reads every register and member back from the owner of its slot and
// writes the final state of each write: s=ok for acknowledged, s=fail for failed, s=info for
// unknown, and f=1 or f=0 for whether the write's effect is there now, wherever that is decided.
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
	port     = "6379"
	stateDir = "/tmp/gnc"
	dataDir  = "/data/gnc"
	ntags    = 16
)

var nodes = []string{"g1", "g2", "g3", "g4", "g5", "g6"}

var addrs = map[string]string{
	"g1": "10.89.0.11", "g2": "10.89.0.12", "g3": "10.89.0.13",
	"g4": "10.89.0.14", "g5": "10.89.0.15", "g6": "10.89.0.16",
}

// nodeOf maps an address (host or host:port) to a node's name.
func nodeOf(a string) string {
	h := a
	if x, _, err := net.SplitHostPort(a); err == nil {
		h = x
	}
	for n, ad := range addrs {
		if ad == h || n == h {
			return n
		}
	}
	return h
}

func shardOf(n string) int {
	i := atoiDef(strings.TrimPrefix(n, "g"), 1)
	return (i-1)%3 + 1
}

func shardNodes(s int) []string { return []string{fmt.Sprintf("g%d", s), fmt.Sprintf("g%d", s+3)} }

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

// writeSynced writes a small file on the data disk and syncs it and its folder, so that a
// recovery boot after a power cut reads it too.
func writeSynced(path, text string) {
	os.MkdirAll(filepath.Dir(path), 0o755)
	tmp := path + ".new"
	f, err := os.Create(tmp)
	if err != nil {
		return
	}
	f.WriteString(text)
	f.Sync()
	f.Close()
	os.Rename(tmp, path)
	if d, err := os.Open(filepath.Dir(path)); err == nil {
		d.Sync()
		d.Close()
	}
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

// event writes an SDK assertion record: the event of a reaction (A=garnet.failover_done:cut).
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

// ---- keys and slots ----

// crc16 is the CRC16 of Redis Cluster (XMODEM).
func crc16(b []byte) uint16 {
	var crc uint16
	for _, c := range b {
		crc ^= uint16(c) << 8
		for i := 0; i < 8; i++ {
			if crc&0x8000 != 0 {
				crc = crc<<1 ^ 0x1021
			} else {
				crc <<= 1
			}
		}
	}
	return crc
}

func tagSlot(t int) int { return int(crc16([]byte(fmt.Sprintf("t%d", t)))) % 16384 }

// tagOf is the hash tag of a name: its number mod 16.
func tagOf(name string) int {
	i := strings.IndexFunc(name, func(r rune) bool { return r >= '0' && r <= '9' })
	if i < 0 {
		return 0
	}
	j := i
	for j < len(name) && name[j] >= '0' && name[j] <= '9' {
		j++
	}
	return atoiDef(name[i:j], 0) % ntags
}

// rkey is the key of a name, with its hash tag.
func rkey(name string) string { return fmt.Sprintf("{t%d}%s", tagOf(name), name) }

func slotOf(name string) int { return tagSlot(tagOf(name)) }

// ---- RESP ----

func client(node string, timeout time.Duration) *redis.Client {
	a := addrs[node]
	if a == "" {
		a = node
	}
	return redis.NewClient(&redis.Options{
		Addr:            net.JoinHostPort(a, port),
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

var errNotSent = errors.New("no node named the owner of the slot: nothing was sent")

// certain says whether an error means that Garnet did nothing.
func certain(err error) bool {
	if errors.Is(err, errNotSent) {
		return true
	}
	var oe *net.OpError
	if errors.As(err, &oe) && oe.Op == "dial" {
		return true // the connection never opened: nothing was sent
	}
	var re redis.Error
	if !errors.As(err, &re) {
		return false
	}
	m := strings.ToUpper(err.Error())
	for _, p := range []string{"MOVED", "ASK ", "MIGRATING", "CLUSTERDOWN", "TRYAGAIN", "CROSSSLOT", "READONLY", "LOADING", "EXECABORT", "WRONGTYPE", "NOSCRIPT"} {
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

// redirect parses a MOVED or ASK reply: the node to go to, and whether it is an ASK.
func redirect(err error) (string, bool, bool) {
	if err == nil {
		return "", false, false
	}
	f := strings.Fields(err.Error())
	if len(f) == 3 && (f[0] == "MOVED" || f[0] == "ASK") {
		return nodeOf(f[2]), f[0] == "ASK", true
	}
	return "", false, false
}

// owners asks the nodes, in an order that starts at a seeded place, for CLUSTER SLOTS, and maps
// every slot of the 16 tags to the node that serves it.
func owners() (map[int]string, string, error) {
	start := cmdID() % len(nodes)
	var last error
	for i := range nodes {
		n := nodes[(start+i)%len(nodes)]
		c := client(n, 3*time.Second)
		ctx, cancel := ctxT(4 * time.Second)
		ss, err := c.ClusterSlots(ctx).Result()
		cancel()
		c.Close()
		if err != nil {
			last = err
			continue
		}
		m := map[int]string{}
		for t := 0; t < ntags; t++ {
			s := tagSlot(t)
			for _, r := range ss {
				if s >= int(r.Start) && s <= int(r.End) && len(r.Nodes) > 0 {
					m[s] = nodeOf(r.Nodes[0].Addr)
				}
			}
		}
		if len(m) == 0 {
			last = fmt.Errorf("%s serves no slot of the test", n)
			continue
		}
		return m, n, nil
	}
	return nil, "", last
}

// onSlot runs fn on the owner of a slot and follows up to three redirects. fn gets asking when
// the node redirected with ASK: it must send ASKING first on the same connection.
func onSlot(slot int, fn func(ctx context.Context, c *redis.Client, asking bool) error) (string, error) {
	own, _, err := owners()
	if err != nil {
		return "", fmt.Errorf("%w: %v", errNotSent, err)
	}
	node := own[slot]
	if node == "" {
		return "", fmt.Errorf("%w: slot %d has no owner", errNotSent, slot)
	}
	asking := false
	for hop := 0; ; hop++ {
		c := client(node, 15*time.Second)
		ctx, cancel := ctxT(20 * time.Second)
		err = fn(ctx, c, asking)
		cancel()
		c.Close()
		to, ask, ok := redirect(err)
		if !ok || hop >= 3 {
			return node, err
		}
		node, asking = to, ask
	}
}

// do1 sends one command, after ASKING when asking.
func do1(ctx context.Context, c *redis.Client, asking bool, args ...any) *redis.Cmd {
	if !asking {
		return c.Do(ctx, args...)
	}
	var cmd *redis.Cmd
	c.Pipelined(ctx, func(p redis.Pipeliner) error {
		p.Do(ctx, "ASKING")
		cmd = p.Do(ctx, args...)
		return nil
	})
	return cmd
}

// ---- the cluster's shape ----

type nodeInfo struct {
	id, name, role, primaryID string
	slots                     []string
	connected, myself, failed bool
}

// clusterNodes reads CLUSTER NODES of one node.
func clusterNodes(n string) ([]nodeInfo, error) {
	c := client(n, 3*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(4 * time.Second)
	defer cancel()
	s, err := c.ClusterNodes(ctx).Result()
	if err != nil {
		return nil, err
	}
	var out []nodeInfo
	for _, l := range strings.Split(s, "\n") {
		f := strings.Fields(l)
		if len(f) < 8 {
			continue
		}
		ni := nodeInfo{id: f[0], name: nodeOf(strings.Split(f[1], "@")[0]), primaryID: f[3], slots: f[8:]}
		for _, fl := range strings.Split(f[2], ",") {
			switch fl {
			case "myself":
				ni.myself = true
			case "master":
				ni.role = "master"
			case "slave":
				ni.role = "slave"
			case "fail", "fail?":
				ni.failed = true
			}
		}
		ni.connected = f[7] == "connected"
		out = append(out, ni)
	}
	return out, nil
}

func myID(n string) (string, error) {
	c := client(n, 3*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(4 * time.Second)
	defer cancel()
	return c.Do(ctx, "CLUSTER", "MYID").Text()
}

// role returns ROLE's first field of a node: master or slave.
func role(n string) (string, error) {
	c := client(n, 3*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(4 * time.Second)
	defer cancel()
	v, err := c.Do(ctx, "ROLE").Slice()
	if err != nil {
		return "", err
	}
	if len(v) == 0 {
		return "", fmt.Errorf("empty ROLE")
	}
	return fmt.Sprint(v[0]), nil
}

// replInfo reads INFO replication into a map.
func replInfo(n string) (map[string]string, error) {
	c := client(n, 3*time.Second)
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

func primaryFile(s int) string { return fmt.Sprintf("%s/primary%d", dataDir, s) }

// shardPrimary is the member of a shard whose ROLE is master; when neither answers so, the
// member that the last failover named.
func shardPrimary(s int) string {
	for _, n := range shardNodes(s) {
		if r, err := role(n); err == nil && r == "master" {
			return n
		}
	}
	if b, err := os.ReadFile(primaryFile(s)); err == nil {
		if p := strings.TrimSpace(string(b)); p != "" {
			return p
		}
	}
	return shardNodes(s)[0]
}

func other(s int, n string) string {
	m := shardNodes(s)
	if m[0] == n {
		return m[1]
	}
	return m[0]
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

// A register's name is its key's name (s<n>, e<n>, x<n>) or h<n>_f for the field f of the hash h<n>.
func regKey(reg string) (key, field string) {
	if k, f, ok := strings.Cut(reg, "_"); ok {
		return rkey(k), f
	}
	return rkey(reg), ""
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
func readReg(ctx context.Context, c *redis.Client, asking bool, reg string) (string, error) {
	k, f := regKey(reg)
	var v string
	var err error
	if f != "" {
		v, err = do1(ctx, c, asking, "HGET", k, f).Text()
	} else {
		v, err = do1(ctx, c, asking, "GET", k).Text()
	}
	if err == redis.Nil {
		return "-", nil
	}
	return v, err
}

// A member write's name names its container: l<n> list, z<n> set, q<n> sorted set, t<n> and
// u<n> the keys of a transaction and of a script.
func hasMember(ctx context.Context, c *redis.Client, kind, name, tag string) (bool, string, error) {
	key := rkey(name)
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
		parts, err := groupParts(ctx, c, kind, key, tag)
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
func groupParts(ctx context.Context, c *redis.Client, kind, key, tag string) ([]bool, error) {
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
	case "set", "setex", "over", "del", "hset", "late":
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
	case "failed":
		hrec(cmdID(), "fail", "send", "writes", o.id())
		if reg && existed {
			saveState(o.key, keyState{o.seq, before.val})
		}
		fmt.Fprintf(os.Stderr, "gnc: %s: failed: %s\n", o.id(), errText(err))
	default:
		hrec(cmdID(), "info", "send", "writes", o.id())
		if reg {
			retire(o.key)
		}
		fmt.Fprintf(os.Stderr, "gnc: %s: unknown: %s\n", o.id(), errText(err))
	}
	return oc
}

func outcome(err error) string {
	switch {
	case err == nil:
		return "ack"
	case certain(err):
		return "failed"
	}
	return "unknown"
}

func runOp(kind string) int {
	seeClock()
	defer seeClock()
	r := rng()
	cmd := cmdID()
	tag := fmt.Sprintf("v%d", cmd)

	var o op
	var before keyState
	existed := false
	failed := "" // a reply that means Garnet did nothing, and why
	var fn func(ctx context.Context, c *redis.Client, asking bool) error

	switch kind {
	case "set", "setex", "hset":
		name := fmt.Sprintf("s%d", cmd)
		switch kind {
		case "setex":
			name = fmt.Sprintf("e%d", cmd)
			ttl := []int64{3, 8, 20, 60, 3600}[r.IntN(5)]
			tag = fmt.Sprintf("v%dt%d", cmd, time.Now().UnixMilli()+ttl*1000)
		case "hset":
			name = fmt.Sprintf("h%d_f", cmd)
		}
		lock(name)
		defer unlock(name)
		o = op{kind, name, 0, tag}
		k, f := regKey(name)
		fn = func(ctx context.Context, c *redis.Client, asking bool) error {
			switch kind {
			case "set":
				return do1(ctx, c, asking, "SET", k, tag).Err()
			case "setex":
				return do1(ctx, c, asking, "SET", k, tag, "PXAT", deadline(tag)).Err()
			}
			return do1(ctx, c, asking, "HSET", k, f, tag).Err()
		}
	case "over", "del":
		prefixes := "sh"
		if kind == "del" {
			prefixes = "s"
		}
		name, s := pickKey(r, prefixes)
		if name == "" {
			return runOp("set")
		}
		defer unlock(name)
		before, existed = s, true
		val := tag
		if kind == "del" {
			val = "-"
		}
		o = op{kind, name, s.seq + 1, val}
		k, f := regKey(name)
		fn = func(ctx context.Context, c *redis.Client, asking bool) error {
			if kind == "del" {
				n, err := do1(ctx, c, asking, "DEL", k).Int64()
				if err == nil && n != 1 {
					failed = fmt.Sprintf("DEL %s answered %d", k, n)
				}
				return err
			}
			if f != "" {
				return do1(ctx, c, asking, "HSET", k, f, tag).Err()
			}
			return do1(ctx, c, asking, "SET", k, tag).Err()
		}
	case "incr":
		name := fmt.Sprintf("c%d", r.IntN(ntags))
		o = op{kind, name, cmd, "+1"}
		fn = func(ctx context.Context, c *redis.Client, asking bool) error {
			return do1(ctx, c, asking, "INCR", rkey(name)).Err()
		}
	case "rpush", "sadd", "zadd":
		pre := map[string]string{"rpush": "l", "sadd": "z", "zadd": "q"}[kind]
		name := fmt.Sprintf("%s%d", pre, r.IntN(ntags))
		o = op{kind, name, 0, tag}
		k := rkey(name)
		fn = func(ctx context.Context, c *redis.Client, asking bool) error {
			switch kind {
			case "rpush":
				return do1(ctx, c, asking, "RPUSH", k, tag).Err()
			case "sadd":
				n, err := do1(ctx, c, asking, "SADD", k, tag).Int64()
				if err == nil && n != 1 {
					failed = fmt.Sprintf("SADD %s %s answered %d", k, tag, n)
				}
				return err
			}
			return do1(ctx, c, asking, "ZADD", k, cmd, tag).Err()
		}
	case "txn":
		name := fmt.Sprintf("t%d", cmd)
		o = op{kind, name, 0, tag}
		k := rkey(name)
		fn = func(ctx context.Context, c *redis.Client, asking bool) error {
			if asking {
				return errors.New("ASK during a transaction: not sent again")
			}
			_, err := c.TxPipelined(ctx, func(p redis.Pipeliner) error {
				p.Set(ctx, k+"a", tag, 0)
				p.HSet(ctx, k+"b", "f", tag)
				p.SAdd(ctx, k+"c", tag)
				p.RPush(ctx, k+"d", tag)
				return nil
			})
			return err
		}
	case "lua":
		name := fmt.Sprintf("u%d", cmd)
		o = op{kind, name, 0, tag}
		k := rkey(name)
		fn = func(ctx context.Context, c *redis.Client, asking bool) error {
			return do1(ctx, c, asking, "EVAL", "redis.call('SET', KEYS[1], ARGV[1]); redis.call('SADD', KEYS[2], ARGV[1]); return 1",
				2, k+"a", k+"c", tag).Err()
		}
	default:
		fmt.Fprintln(os.Stderr, "gnc: unknown operation", kind)
		return 2
	}

	begin(o)
	_, err := onSlot(slotOf(o.key), fn)
	if failed != "" {
		finish(o, "failed", errors.New(failed), before, existed)
		if kind == "del" {
			// the register's last value is known, so the reply contradicts what was acknowledged
			v := fmt.Sprintf("read-your-writes: %s: %s, but %s's last acknowledged value is %s", o.id(), failed, o.key, before.val)
			console("gnc: " + v)
			fmt.Println(v)
			return 100
		}
		return 1
	}
	if finish(o, outcome(err), err, before, existed) != "ack" {
		return 1
	}
	return 0
}

// ---- the failover ----

// failover runs CLUSTER FAILOVER (the default, which waits for the replica to catch up) on the
// replica of one shard, waits until it is the primary, and sends one late write to the old
// primary, as a client does that has not learned of the failover yet. It runs in one call of two,
// by the seed.
func failover() int {
	r := rng()
	if r.IntN(2) != 0 {
		return 0
	}
	s := 1 + r.IntN(3)
	old := shardPrimary(s)
	rep := other(s, old)
	if rl, err := role(rep); err != nil || rl != "slave" {
		fmt.Printf("gnc: failover: %s is not a replica now (%s %v): no failover\n", rep, rl, err)
		return 0
	}
	if rl, err := role(old); err != nil || rl != "master" {
		fmt.Printf("gnc: failover: %s is not a primary now (%s %v): no failover\n", old, rl, err)
		return 0
	}
	c := client(rep, 10*time.Second)
	ctx, cancel := ctxT(15 * time.Second)
	err := c.Do(ctx, "CLUSTER", "FAILOVER").Err()
	cancel()
	c.Close()
	if err != nil {
		fmt.Printf("gnc: failover: CLUSTER FAILOVER on %s: %s\n", rep, errText(err))
		return 0
	}
	event("garnet.failover_started", "a replica accepted CLUSTER FAILOVER")
	until := time.Now().Add(60 * time.Second)
	for {
		if rl, err := role(rep); err == nil && rl == "master" {
			break
		}
		if time.Now().After(until) {
			info, _ := replInfo(rep)
			fmt.Printf("gnc: failover: %s is not the primary 60 s after CLUSTER FAILOVER (%s, %s)\n", rep, info["master_failover_state"], info["last_failover_state"])
			return 0
		}
		time.Sleep(500 * time.Millisecond)
	}
	writeSynced(primaryFile(s), rep+"\n")
	event("garnet.failover_done", "the replica is the primary")
	console(fmt.Sprintf("gnc: failover of shard %d from %s to %s", s, old, rep))
	lateWrite(old)
	fmt.Printf("gnc: failover of shard %d from %s to %s\n", s, old, rep)
	return 0
}

// lateWrite is the write of a client that has not learned of the failover yet: a SET to the old
// primary, without following its redirect. A redirect means that it did nothing; an OK is an
// acknowledgement like any other.
func lateWrite(old string) {
	name := fmt.Sprintf("x%d", cmdID())
	o := op{"late", name, 0, fmt.Sprintf("v%dx", cmdID())}
	lock(name)
	defer unlock(name)
	begin(o)
	k, _ := regKey(name)
	c := client(old, 5*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(8 * time.Second)
	defer cancel()
	err := c.Set(ctx, k, o.val, 0).Err()
	if err == nil {
		event("garnet.late_write_acked", "the old primary acknowledged a write after the failover")
	}
	finish(o, outcome(err), err, keyState{}, false)
}

// ---- the slot migration ----

// migrate moves the slot of one hash tag from its primary to another primary with MIGRATE ...
// SLOTS, and waits until the target serves it.
func migrate() int {
	r := rng()
	t := r.IntN(ntags)
	slot := tagSlot(t)
	own, _, err := owners()
	if err != nil {
		fmt.Printf("gnc: migrate: %s\n", errText(err))
		return 0
	}
	src := own[slot]
	var cands []string
	for s := 1; s <= 3; s++ {
		if p := shardPrimary(s); p != src {
			if rl, err := role(p); err == nil && rl == "master" {
				cands = append(cands, p)
			}
		}
	}
	if len(cands) == 0 || src == "" {
		fmt.Printf("gnc: migrate: no target for slot %d of %s\n", slot, src)
		return 0
	}
	sort.Strings(cands)
	dst := cands[r.IntN(len(cands))]
	c := client(src, 30*time.Second)
	ctx, cancel := ctxT(35 * time.Second)
	err = c.Do(ctx, "MIGRATE", addrs[dst], port, "", 0, 5000, "SLOTS", slot).Err()
	cancel()
	c.Close()
	if err != nil {
		fmt.Printf("gnc: migrate: MIGRATE of slot %d from %s to %s: %s\n", slot, src, dst, errText(err))
		return 0
	}
	event("garnet.migrate_started", "a primary accepted MIGRATE ... SLOTS")
	until := time.Now().Add(60 * time.Second)
	for {
		a, b := slotOwnerAt(src, slot), slotOwnerAt(dst, slot)
		if a == dst && b == dst {
			break
		}
		if time.Now().After(until) {
			fmt.Printf("gnc: migrate: slot %d (tag t%d) is not on %s 60 s after MIGRATE: %s says %s, %s says %s\n", slot, t, dst, src, a, dst, b)
			return 0
		}
		time.Sleep(500 * time.Millisecond)
	}
	event("garnet.migrate_done", "the target serves the slot")
	console(fmt.Sprintf("gnc: slot %d (tag t%d) moved from %s to %s", slot, t, src, dst))
	fmt.Printf("gnc: slot %d (tag t%d) moved from %s to %s\n", slot, t, src, dst)
	return 0
}

// slotOwnerAt is the owner of a slot in one node's CLUSTER SLOTS.
func slotOwnerAt(n string, slot int) string {
	c := client(n, 3*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(4 * time.Second)
	defer cancel()
	ss, err := c.ClusterSlots(ctx).Result()
	if err != nil {
		return "?"
	}
	for _, r := range ss {
		if slot >= int(r.Start) && slot <= int(r.End) && len(r.Nodes) > 0 {
			return nodeOf(r.Nodes[0].Addr)
		}
	}
	return "-"
}

// save takes a checkpoint of one primary with BGSAVE; its replica takes its own when the
// checkpoint's record reaches it.
func save() int {
	r := rng()
	p := shardPrimary(1 + r.IntN(3))
	c := client(p, 15*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(20 * time.Second)
	defer cancel()
	if err := c.Do(ctx, "BGSAVE").Err(); err != nil {
		fmt.Fprintf(os.Stderr, "gnc: BGSAVE on %s: %s\n", p, errText(err))
		return 1
	}
	event("garnet.bgsave", "a primary started a checkpoint")
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

// readSome reads up to five registers and containers through one node, any role (READONLY on a
// replica). Each value must be one that some write sent: no node serves a value that no write
// sent.
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
	ctx, cancel := ctxT(20 * time.Second)
	defer cancel()
	if c.Do(ctx, "READONLY").Err() != nil {
		return 0
	}
	for _, k := range keys {
		var vals []string
		var err error
		switch k[0] {
		case 's', 'e', 'h', 'x':
			var v string
			v, err = readReg(ctx, c, false, k)
			vals = []string{v}
		case 'l':
			vals, err = c.LRange(ctx, rkey(k), 0, -1).Result()
		case 'z':
			vals, err = c.SMembers(ctx, rkey(k)).Result()
		case 'q':
			vals, err = c.ZRange(ctx, rkey(k), 0, -1).Result()
		}
		if err != nil {
			continue // a redirect or no answer: nothing to judge
		}
		for _, v := range vals {
			if !w[k][v] {
				// a write may have started after this command read the outcomes
				if w2 := written(); w2[k][v] {
					continue
				}
				msg := fmt.Sprintf("phantom read: %s answered %s for %s, which no write sent", n, v, rkey(k))
				console("gnc: " + msg)
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
// backward clock step expired as set, and Garnet may have removed it, so the value is required
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

const maxClockFile = dataDir + "/maxclock"

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

// waitServed waits up to the deadline until every slot of the test has an owner that answers
// PING, and returns the owners.
func waitServed(until time.Time) (map[int]string, string) {
	for {
		own, _, err := owners()
		why := errText(err)
		if err == nil {
			why = ""
			for t := 0; t < ntags && why == ""; t++ {
				n := own[tagSlot(t)]
				if n == "" {
					why = fmt.Sprintf("slot %d has no owner", tagSlot(t))
					continue
				}
				if cmdReady(n) != 0 {
					why = fmt.Sprintf("%s, the owner of slot %d, does not answer", n, tagSlot(t))
				}
			}
			if why == "" {
				return own, ""
			}
		}
		if time.Now().After(until) {
			return nil, why
		}
		time.Sleep(2 * time.Second)
	}
}

// readBack is the final: of the messages check writes.
func readBack() int {
	const proc = 100000
	seeClock()
	by, oc := opsByKey()
	own, why := waitServed(time.Now().Add(120 * time.Second))
	if own == nil {
		fmt.Printf("the cluster does not serve every slot of the test: %s\n", why)
		return 0
	}
	cl := map[string]*redis.Client{}
	get := func(n string) *redis.Client {
		if cl[n] == nil {
			cl[n] = client(n, 10*time.Second)
		}
		return cl[n]
	}
	defer func() {
		for _, c := range cl {
			c.Close()
		}
	}()
	var keys []string
	for k := range by {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var ids, items, unread []string
	for _, k := range keys {
		ops := by[k]
		c := get(own[slotOf(k)])
		ctx, cancel := ctxT(10 * time.Second)
		if isRegister(ops[0].kind) {
			v, err := readReg(ctx, c, false, k)
			cancel()
			if err != nil {
				for _, o := range ops {
					unread = append(unread, o.id())
				}
				fmt.Printf("%s on %s: %s\n", rkey(k), own[slotOf(k)], errText(err))
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
				fmt.Printf("%s on %s holds %s, which no acknowledged or unknown write left there (expected %s)\n", rkey(k), own[slotOf(k)], v, strings.Join(cs, " or "))
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
			has, odd, err := hasMember(ctx, c, o.kind, o.key, o.val)
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
		cancel()
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
	fmt.Printf("read back %d writes of %d keys from the owners of their slots\n", len(items), len(keys))
	return 0
}

// ---- final checks ----

// dumpKey is a key's type and contents, in one string.
func dumpKey(ctx context.Context, c *redis.Client, k string) (string, error) {
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

// allKeys lists every key of a node with SCAN (READONLY first, so a replica answers too).
func allKeys(ctx context.Context, c *redis.Client) ([]string, error) {
	if err := c.Do(ctx, "READONLY").Err(); err != nil {
		return nil, err
	}
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
func nearDeadline(ctx context.Context, c *redis.Client, k string, now int64) bool {
	if !strings.Contains(k, "}e") {
		return false
	}
	v, err := c.Get(ctx, k).Result()
	if err != nil {
		return false
	}
	d := deadline(v)
	return d != 0 && now > d-3000 && now < d+3000
}

// dumpNode reads every key of one node with its contents. A key that SCAN lists but that the
// node does not serve (a redirect: its slot belongs to another shard) is an orphan: it is in the
// node's store, and it comes back if the slot comes back.
func dumpNode(n string) (map[string]string, []string, error) {
	c := client(n, 10*time.Second)
	defer c.Close()
	ctx, cancel := ctxT(60 * time.Second)
	defer cancel()
	ks, err := allKeys(ctx, c)
	if err != nil {
		return nil, nil, err
	}
	now := time.Now().UnixMilli()
	out := map[string]string{}
	var orphans []string
	for _, k := range ks {
		if nearDeadline(ctx, c, k, now) {
			continue
		}
		d, err := dumpKey(ctx, c, k)
		if _, _, ok := redirect(err); ok {
			orphans = append(orphans, k)
			continue
		}
		if err != nil {
			return nil, nil, err
		}
		if d != "-" {
			out[k] = d
		}
	}
	return out, orphans, nil
}

// orphanText names up to five orphans with their slots.
func orphanText(os []string) string {
	var b []string
	for i, k := range os {
		if i == 5 {
			b = append(b, fmt.Sprintf("and %d more", len(os)-5))
			break
		}
		b = append(b, fmt.Sprintf("%s (slot %d)", k, slotOfKey(k)))
	}
	return strings.Join(b, ", ")
}

// slotOfKey is the slot of a key of the test, from its hash tag.
func slotOfKey(k string) int {
	end := strings.Index(k, "}")
	if !strings.HasPrefix(k, "{t") || end < 0 {
		return -1
	}
	return tagSlot(atoiDef(k[2:end], 0))
}

// checkConverge: after the heal every shard has one primary and one replica that follows it, and
// the replica holds what the primary holds, key for key.
func checkConverge() int {
	bad := 0
	for s := 1; s <= 3; s++ {
		p := shardPrimary(s)
		r := other(s, p)
		pr, err := role(p)
		if err != nil || pr != "master" {
			fmt.Printf("shard %d: %s is not a primary (%s %s)\n", s, p, pr, errText(err))
			return 1
		}
		info, err := replInfo(r)
		if err != nil {
			fmt.Printf("shard %d: the replica %s does not answer: %s\n", s, r, errText(err))
			return 1
		}
		if info["role"] != "slave" || info["master_link_status"] != "up" || nodeOf(info["master_host"]) != p {
			fmt.Printf("shard %d: %s does not follow %s: role %s, master %s, link %s, sync %s\n", s, r, p,
				info["role"], info["master_host"], info["master_link_status"], info["master_sync_in_progress"])
			return 1
		}
		before := bad
		want, po, err := dumpNode(p)
		if err != nil {
			fmt.Printf("shard %d: SCAN on %s: %s\n", s, p, errText(err))
			return 2
		}
		got, ro, err := dumpNode(r)
		if err != nil {
			fmt.Printf("shard %d: SCAN on %s: %s\n", s, r, errText(err))
			return 2
		}
		if len(po) > 0 {
			bad++
			fmt.Printf("the primary %s holds %d keys of slots that its shard does not own: %s\n", p, len(po), orphanText(po))
		}
		if len(ro) > 0 {
			bad++
			fmt.Printf("the replica %s holds %d keys of slots that its shard does not own: %s\n", r, len(ro), orphanText(ro))
		}
		for k, d := range got {
			if w, ok := want[k]; !ok {
				if bad++; bad <= 5 {
					fmt.Printf("the replica %s holds %s = %s, which its primary %s does not hold\n", r, k, d, p)
				}
			} else if w != d {
				if bad++; bad <= 5 {
					fmt.Printf("the replica %s holds %s = %s, and its primary %s holds %s\n", r, k, d, p, w)
				}
			}
		}
		for k, w := range want {
			if _, ok := got[k]; !ok {
				if bad++; bad <= 5 {
					fmt.Printf("the replica %s lacks %s, which its primary %s holds as %s\n", r, k, p, w)
				}
			}
		}
		if bad > before {
			pi, _ := replInfo(p)
			ri, _ := replInfo(r)
			fmt.Printf("shard %d: the primary %s has master_repl_offset %s and replid %s; the replica %s has master_repl_offset %s, slave_read_repl_offset %s, replid %s, link %s\n",
				s, p, pi["master_repl_offset"], pi["master_replid"], r, ri["master_repl_offset"], ri["slave_read_repl_offset"], ri["master_replid"], ri["master_link_status"])
		}
	}
	if bad > 0 {
		fmt.Printf("a replica differs from its primary in %d keys\n", bad)
		return 1
	}
	return 0
}

// checkSlots: after the heal every node names the same owner for every slot of the test, that
// owner is a primary, and no primary holds a key of a slot that it does not own (a key that a
// migration left behind comes back when the slot moves back).
func checkSlots() int {
	var ref map[int]string
	refNode := ""
	for _, n := range nodes {
		m := map[int]string{}
		for t := 0; t < ntags; t++ {
			m[tagSlot(t)] = slotOwnerAt(n, tagSlot(t))
		}
		if ref == nil {
			ref, refNode = m, n
			continue
		}
		for s, o := range m {
			if o != ref[s] {
				fmt.Printf("%s names %s as the owner of slot %d, and %s names %s\n", n, o, s, refNode, ref[s])
				return 1
			}
		}
	}
	for s, o := range ref {
		if r, err := role(o); err != nil || r != "master" {
			fmt.Printf("every node names %s as the owner of slot %d, and %s is %s %s\n", o, s, o, r, errText(err))
			return 1
		}
	}
	return 0
}

// checkAtomic: on every node each transaction and script has all of its writes or none.
func checkAtomic() int {
	by, _ := opsByKey()
	bad := 0
	for _, n := range nodes {
		c := client(n, 10*time.Second)
		ctx, cancel := ctxT(60 * time.Second)
		if c.Do(ctx, "READONLY").Err() != nil {
			cancel()
			c.Close()
			continue
		}
		for _, ops := range by {
			for _, o := range ops {
				if o.kind != "txn" && o.kind != "lua" {
					continue
				}
				_, odd, err := hasMember(ctx, c, o.kind, o.key, o.val)
				if err != nil {
					continue // MOVED: the node does not hold the slot
				}
				if odd != "" {
					if bad++; bad <= 5 {
						fmt.Printf("%s: %s\n", n, odd)
					}
				}
			}
		}
		cancel()
		c.Close()
	}
	if bad > 0 {
		fmt.Printf("%d transactions or scripts were applied in part\n", bad)
		return 1
	}
	return 0
}

// checkCounters: each counter is at least its acknowledged INCRs and at most those and the
// unknown ones.
func checkCounters() int {
	by, oc := opsByKey()
	own, why := waitServed(time.Now().Add(30 * time.Second))
	if own == nil {
		fmt.Printf("the cluster does not serve every slot of the test: %s\n", why)
		return 2
	}
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
		n := own[slotOf(k)]
		c := client(n, 5*time.Second)
		ctx, cancel := ctxT(5 * time.Second)
		v, err := c.Get(ctx, rkey(k)).Int()
		cancel()
		c.Close()
		if err == redis.Nil {
			v, err = 0, nil
		}
		if err != nil {
			return 2
		}
		if v < acked || v > acked+unknown {
			bad++
			fmt.Printf("%s on %s is %d, with %d acknowledged and %d unknown INCRs\n", rkey(k), n, v, acked, unknown)
		}
	}
	if bad > 0 {
		return 1
	}
	return 0
}

func cmdDump(node string) int {
	m, orphans, err := dumpNode(node)
	if err != nil {
		fmt.Println(errText(err))
		return 2
	}
	for _, k := range orphans {
		fmt.Printf("%s orphan (slot %d, not served here)\n", k, slotOfKey(k))
	}
	var ks []string
	for k := range m {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	for _, k := range ks {
		fmt.Printf("%s %s\n", k, m[k])
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

// cmdInit forms the cluster as the cluster's documentation does: a config epoch per node, the
// slots in three ranges on g1, g2 and g3, CLUSTER MEET from g1 to the others, and CLUSTER
// REPLICATE on g4, g5 and g6. It then waits until each replica's link to its primary is up.
// A cluster that is formed already (the file /data/gnc/formed) is only waited for.
func cmdInit() int {
	formed := dataDir + "/formed"
	until := time.Now().Add(170 * time.Second)
	if _, err := os.Stat(formed); err != nil {
		ids := map[string]string{}
		for _, n := range nodes {
			id, err := myID(n)
			if err != nil {
				fmt.Printf("gnc: CLUSTER MYID on %s: %s\n", n, errText(err))
				return 1
			}
			ids[n] = id
		}
		for i, n := range nodes {
			c := client(n, 5*time.Second)
			ctx, cancel := ctxT(6 * time.Second)
			if err := c.Do(ctx, "CLUSTER", "SET-CONFIG-EPOCH", i+1).Err(); err != nil {
				fmt.Printf("gnc: CLUSTER SET-CONFIG-EPOCH on %s: %s\n", n, errText(err))
			}
			cancel()
			c.Close()
		}
		ranges := [][2]int{{0, 5460}, {5461, 10922}, {10923, 16383}}
		for i, r := range ranges {
			c := client(nodes[i], 5*time.Second)
			ctx, cancel := ctxT(6 * time.Second)
			err := c.Do(ctx, "CLUSTER", "ADDSLOTSRANGE", r[0], r[1]).Err()
			cancel()
			c.Close()
			if err != nil {
				fmt.Printf("gnc: CLUSTER ADDSLOTSRANGE on %s: %s\n", nodes[i], errText(err))
				return 1
			}
		}
		c := client("g1", 5*time.Second)
		for _, n := range nodes[1:] {
			ctx, cancel := ctxT(6 * time.Second)
			err := c.Do(ctx, "CLUSTER", "MEET", addrs[n], port).Err()
			cancel()
			if err != nil {
				fmt.Printf("gnc: CLUSTER MEET %s: %s\n", n, errText(err))
				c.Close()
				return 1
			}
		}
		c.Close()
		// wait until every node knows all six
		for {
			ok := true
			for _, n := range nodes {
				ns, err := clusterNodes(n)
				if err != nil || len(ns) < len(nodes) {
					ok = false
					break
				}
			}
			if ok {
				break
			}
			if time.Now().After(until) {
				fmt.Println("gnc: the nodes do not know each other")
				return 1
			}
			time.Sleep(time.Second)
		}
		for i, n := range nodes[3:] {
			for {
				c := client(n, 10*time.Second)
				ctx, cancel := ctxT(15 * time.Second)
				err := c.Do(ctx, "CLUSTER", "REPLICATE", ids[nodes[i]]).Err()
				cancel()
				c.Close()
				if err == nil {
					break
				}
				if time.Now().After(until) {
					fmt.Printf("gnc: CLUSTER REPLICATE on %s: %s\n", n, errText(err))
					return 1
				}
				time.Sleep(time.Second)
			}
		}
		for s := 1; s <= 3; s++ {
			writeSynced(primaryFile(s), fmt.Sprintf("g%d\n", s))
		}
		writeSynced(formed, "yes\n")
	}
	for {
		why := ""
		for s := 1; s <= 3 && why == ""; s++ {
			p := shardPrimary(s)
			info, err := replInfo(other(s, p))
			if err != nil {
				why = errText(err)
			} else if info["master_link_status"] != "up" || nodeOf(info["master_host"]) != p {
				why = fmt.Sprintf("shard %d: %s follows %s, link %s", s, other(s, p), info["master_host"], info["master_link_status"])
			}
		}
		if why == "" {
			fmt.Println("gnc: three shards, each with its replica in step")
			return 0
		}
		if time.Now().After(until) {
			fmt.Printf("gnc: the cluster is not formed after 170 s: %s\n", why)
			return 1
		}
		time.Sleep(2 * time.Second)
	}
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: gnc ready <addr> | init | primary <shard> | op <kind> | failover | migrate | save | read | readback | check <rule> | dump <node>")
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
		fmt.Println(shardPrimary(atoiDef(arg, 1)))
		os.Exit(0)
	case "op":
		os.Exit(runOp(arg))
	case "failover":
		os.Exit(failover())
	case "migrate":
		os.Exit(migrate())
	case "save":
		os.Exit(save())
	case "read":
		os.Exit(readSome())
	case "readback":
		os.Exit(readBack())
	case "check":
		switch arg {
		case "converge":
			os.Exit(checkConverge())
		case "slots":
			os.Exit(checkSlots())
		case "atomic":
			os.Exit(checkAtomic())
		case "counters":
			os.Exit(checkCounters())
		}
	case "dump":
		os.Exit(cmdDump(arg))
	}
	fmt.Fprintln(os.Stderr, "gnc: unknown command", os.Args[1:])
	os.Exit(2)
}
