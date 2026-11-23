// gac is the test client of the Garage example: the cluster's setup, the workload's operations
// and the checks. It speaks S3 with the AWS SDK for Go v2 and Garage's admin API v2 with Go's
// standard library.
//
//	gac init              connect the three nodes, apply the layout, import the key, make the bucket
//	gac op <kind>         one S3 operation; the composer's HYDO_* variables drive it
//	gac read              read some objects and check that each one is whole (anytime_read)
//	gac readback          the final: of the messages check: each operation's effect, read back
//	gac check <rule>      one final check: list (ListObjectsV2) or uploads (ListMultipartUploads)
//	gac get <key>         print what each node answers for one key (for a look by hand)
//	gac list [<node>]     print the keys that ListObjectsV2 through one node lists (no GET)
//
// The nodes are garage1, garage2 and garage3 (S3 on port 3900, the admin API on 3903). Every
// operation goes to a node that the command's seed picks, and the read after an acknowledged
// write goes to another node.
//
// Objects. Every object body starts with one line, "gac <tag> <n> <sha256>", followed by n bytes
// that the tag's seed makes; the sha256 is that of the n bytes. A read is whole when the line
// matches the bytes after it. The tag v<cmd>x<i> is unique per write; a copy carries the tag of
// its source. A key's value is the tag of its body, "-" when the key does not exist, or
// "corrupt" when the body is not whole.
//
// Operations, one id each: <kind>.<key>.<seq>.<value>, where seq counts the operations on the
// key and value is the tag the operation writes ("-" for a deletion and an aborted upload):
//
//	put    PutObject of a new key
//	over   PutObject over an existing key
//	del    DeleteObject of an existing key
//	mdel   DeleteObjects of two existing keys (one id per key)
//	mpu    a multipart upload of 2 to 4 parts to a new or an existing key, then its completion
//	abort  a multipart upload of one part to a new key, then its abort
//	copy   CopyObject of an existing key to a new or an existing key
//
// One command at a time works on a key (a lock file under /tmp/gac/lock). A key's last known
// value and seq are in /tmp/gac/keys/<key>, for the keys whose every operation got a certain
// answer. A key whose operation got no certain answer is never written again, so each key ends
// with at most one operation of unknown outcome.
//
// Outcomes. A 2xx answer is acknowledged. A 4xx answer but 408 and 429 is failed: Garage did not
// apply it. Every other answer, and no answer, is unknown. $HYDO_ACKED gets the id of each
// acknowledged operation, $HYDO_OUTCOMES gets "<id> ack|failed|unknown".
//
// The history objects (hydo guide messages): each operation is a send of its id to the channel
// objects, with the answer's ETag as its system id. `gac readback` reads every key back and
// writes the final state of each operation: s=ok for acknowledged, s=fail for failed, s=info for
// unknown, and f=1 or f=0 for whether the operation's value is the key's value now, wherever
// that is decided.
package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	awshttp "github.com/aws/aws-sdk-go-v2/aws/transport/http"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
)

const (
	bucket     = "hydo"
	accessKey  = "GK1a2b3c4d5e6f708192a3b4c5"
	secretKey  = "4f1d9e2c7b8a6f5e3d2c1b0a99887766554433221100ffeeddccbbaa99887766"
	adminToken = "hydo-garage-admin-token"
	stateDir   = "/tmp/gac"
)

var nodes = []string{"garage1", "garage2", "garage3"}

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

// console writes one line to the guest's console, where reactions (A=) and the run's log see it.
func console(line string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	f.WriteString(line + "\n")
}

// event writes an SDK assertion record: the event of a reaction (A=garage.put_acked:cut).
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

// hrec writes one record of the history objects: HYDO-HIST objects <process> <type> <time> <args>.
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
	f.WriteString(fmt.Sprintf("HYDO-HIST objects %d %s %s %s\n", proc, typ, uptimeNs(), strings.Join(args, " ")))
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

// ---- S3 ----

func client(node string, timeout time.Duration) *s3.Client {
	return s3.New(s3.Options{
		Region:       "garage",
		BaseEndpoint: aws.String("http://" + node + ":3900"),
		UsePathStyle: true,
		Credentials: aws.CredentialsProviderFunc(func(context.Context) (aws.Credentials, error) {
			return aws.Credentials{AccessKeyID: accessKey, SecretAccessKey: secretKey}, nil
		}),
		Retryer:    aws.NopRetryer{},
		HTTPClient: &http.Client{Timeout: timeout},
	})
}

// status is the HTTP status of an S3 error, or 0 when no answer came.
func status(err error) int {
	var re *awshttp.ResponseError
	if errors.As(err, &re) {
		return re.HTTPStatusCode()
	}
	return 0
}

// outcome classifies an error: "failed" for a 4xx answer but 408 and 429, else "unknown".
func outcome(err error) string {
	if st := status(err); st >= 400 && st < 500 && st != 408 && st != 429 {
		return "failed"
	}
	return "unknown"
}

func errText(err error) string {
	if st := status(err); st != 0 {
		var ae interface{ ErrorCode() string }
		if errors.As(err, &ae) {
			return fmt.Sprintf("%d %s", st, ae.ErrorCode())
		}
		return fmt.Sprintf("%d", st)
	}
	s := err.Error()
	if len(s) > 160 {
		s = s[len(s)-160:]
	}
	return "no answer: " + s
}

// body makes the body of tag: the header line and n bytes from the tag's seed.
func body(tag string, n int) []byte {
	h := sha256.Sum256([]byte(tag))
	r := rand.New(rand.NewPCG(uint64(h[0])<<56|uint64(h[1])<<48|uint64(h[2])<<40|uint64(h[3])<<32, uint64(h[4])))
	data := make([]byte, n)
	for i := range data {
		data[i] = byte('a' + r.IntN(26))
	}
	sum := sha256.Sum256(data)
	return append([]byte(fmt.Sprintf("gac %s %d %s\n", tag, n, hex.EncodeToString(sum[:]))), data...)
}

// valueOf reads a body: its tag when it is whole, "corrupt" otherwise.
func valueOf(b []byte) string {
	i := bytes.IndexByte(b, '\n')
	if i < 0 {
		return "corrupt"
	}
	f := strings.Fields(string(b[:i]))
	if len(f) != 4 || f[0] != "gac" {
		return "corrupt"
	}
	rest := b[i+1:]
	if atoiDef(f[2], -1) != len(rest) {
		return "corrupt"
	}
	sum := sha256.Sum256(rest)
	if hex.EncodeToString(sum[:]) != f[3] {
		return "corrupt"
	}
	return f[1]
}

// size draws an object size: most small (inline in Garage's metadata, under 3 KiB), some of
// tens of KiB (one data block), a few over Garage's 1 MiB block size (two data blocks).
func size(r *rand.Rand) int {
	switch x := r.IntN(10); {
	case x < 5:
		return 64 + r.IntN(2000)
	case x < 9:
		return 8192 + r.IntN(120000)
	default:
		return 1100000 + r.IntN(300000)
	}
}

// readKey reads key through node: its value ("-" for none), or an error when no certain answer came.
func readKey(node, key string, timeout time.Duration) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	out, err := client(node, timeout).GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(bucket), Key: aws.String(key)})
	if err != nil {
		if status(err) == 404 {
			return "-", nil
		}
		return "", err
	}
	defer out.Body.Close()
	b, err := io.ReadAll(out.Body)
	if err != nil {
		return "", err
	}
	return valueOf(b), nil
}

// ---- keys, their locks and their state ----

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

// pickKey locks a known key that exists now (want "obj") or any known key (want ""), or returns "".
func pickKey(r *rand.Rand, want string, not string) (string, keyState) {
	ents, _ := os.ReadDir(filepath.Join(stateDir, "keys"))
	var names []string
	for _, e := range ents {
		if !strings.HasPrefix(e.Name(), ".") && e.Name() != not {
			names = append(names, e.Name())
		}
	}
	sort.Strings(names)
	r.Shuffle(len(names), func(i, j int) { names[i], names[j] = names[j], names[i] })
	for _, k := range names {
		if !lock(k) {
			continue
		}
		s, ok := loadState(k)
		if ok && (want == "" || s.val != "-") {
			return k, s
		}
		unlock(k)
	}
	return "", keyState{}
}

// ---- one operation ----

type op struct {
	kind, key string
	seq       int
	val       string
	sub       int // the i-th key of one DeleteObjects request
}

// proc is the operation's history process: a process has one operation in flight, so the
// second key of a DeleteObjects request is the command's id plus 1000000.
func (o op) proc() int { return cmdID() + 1000000*o.sub }

func (o op) id() string { return fmt.Sprintf("%s.%s.%d.%s", o.kind, o.key, o.seq, o.val) }

func parseID(id string) (op, bool) {
	f := strings.Split(id, ".")
	if len(f) != 4 {
		return op{}, false
	}
	return op{f[0], f[1], atoiDef(f[2], -1), f[3], 0}, true
}

// begin records the operation's start. Its outcome is unknown until finish records another, so
// a command that a power cut or the composer stops leaves it unknown.
func begin(o op) {
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" unknown")
	hrec(o.proc(), "invoke", "send", "objects", o.id())
}

// finish records the outcome of o and updates the key's state. It returns the outcome.
func finish(o op, etag string, err error, before keyState, existed bool) string {
	oc := "ack"
	if err != nil {
		oc = outcome(err)
	}
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" "+oc)
	switch oc {
	case "ack":
		appendLine(os.Getenv("HYDO_ACKED"), o.id())
		it := o.id()
		if etag != "" {
			it += ";o=" + enc(strings.Trim(etag, "\""))
		}
		hrec(o.proc(), "ok", "send", "objects", it)
		saveState(o.key, keyState{o.seq, o.val})
		event("garage."+o.kind+"_acked", "Garage acknowledged a "+o.kind)
	case "failed":
		hrec(o.proc(), "fail", "send", "objects", o.id())
		if existed {
			saveState(o.key, keyState{o.seq, before.val})
		}
		fmt.Fprintf(os.Stderr, "gac: %s: %s\n", o.id(), errText(err))
	default:
		hrec(o.proc(), "info", "send", "objects", o.id())
		retire(o.key)
		fmt.Fprintf(os.Stderr, "gac: %s: %s\n", o.id(), errText(err))
	}
	return oc
}

// readAfterWrite reads the key through another node than the writer's, until a certain answer
// or 3 tries. A certain answer other than the written value breaks read-after-write: it returns
// a description then.
func readAfterWrite(o op, writer string, r *rand.Rand) string {
	others := []string{}
	for _, n := range nodes {
		if n != writer {
			others = append(others, n)
		}
	}
	for try := 0; try < 3; try++ {
		n := others[r.IntN(len(others))]
		v, err := readKey(n, o.key, 10*time.Second)
		if err != nil {
			time.Sleep(time.Second)
			continue
		}
		if v != o.val {
			return fmt.Sprintf("read-after-write: %s was acknowledged by %s, and then %s answered %s for %s", o.id(), writer, n, v, o.key)
		}
		return ""
	}
	return ""
}

func put(c *s3.Client, key string, b []byte) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	out, err := c.PutObject(ctx, &s3.PutObjectInput{Bucket: aws.String(bucket), Key: aws.String(key), Body: bytes.NewReader(b)})
	if err != nil {
		return "", err
	}
	return aws.ToString(out.ETag), nil
}

func multipart(c *s3.Client, key string, b []byte, nparts int) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	cr, err := c.CreateMultipartUpload(ctx, &s3.CreateMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key)})
	if err != nil {
		return "", errNotDone{err}
	}
	var parts []types.CompletedPart
	step := (len(b) + nparts - 1) / nparts
	for i := 0; i < nparts; i++ {
		lo, hi := i*step, min((i+1)*step, len(b))
		up, err := c.UploadPart(ctx, &s3.UploadPartInput{Bucket: aws.String(bucket), Key: aws.String(key),
			UploadId: cr.UploadId, PartNumber: aws.Int32(int32(i + 1)), Body: bytes.NewReader(b[lo:hi])})
		if err != nil {
			c.AbortMultipartUpload(context.Background(), &s3.AbortMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key), UploadId: cr.UploadId})
			return "", errNotDone{err}
		}
		parts = append(parts, types.CompletedPart{ETag: up.ETag, PartNumber: aws.Int32(int32(i + 1))})
	}
	out, err := c.CompleteMultipartUpload(ctx, &s3.CompleteMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key),
		UploadId: cr.UploadId, MultipartUpload: &types.CompletedMultipartUpload{Parts: parts}})
	if err != nil {
		return "", err
	}
	return aws.ToString(out.ETag), nil
}

// errNotDone wraps an error of a step before a multipart upload's completion: the object was
// certainly not written.
type errNotDone struct{ err error }

func (e errNotDone) Error() string { return e.err.Error() }
func (e errNotDone) Unwrap() error { return e.err }

func runOp(kind string) int {
	r := rng()
	cmd := cmdID()
	tag := fmt.Sprintf("v%dx0", cmd)
	writer := nodes[r.IntN(len(nodes))]
	c := client(writer, 30*time.Second)

	var o op
	var before keyState
	existed := false
	var etag string
	var err error

	switch kind {
	case "put", "over", "mpu":
		key := ""
		if kind == "over" || (kind == "mpu" && r.IntN(2) == 0) {
			key, before = pickKey(r, "", "")
			existed = key != ""
		}
		if key == "" {
			key = fmt.Sprintf("k%d", cmd)
			lock(key)
		}
		defer unlock(key)
		o = op{kind, key, before.seq + 1, tag, 0}
		if !existed {
			o.seq = 0
		}
		b := body(tag, size(r))
		begin(o)
		if kind == "mpu" {
			etag, err = multipart(c, key, b, 2+r.IntN(3))
			var nd errNotDone
			if errors.As(err, &nd) {
				// a step before the completion failed: the object was not written
				return finishNotDone(o, nd.err, before, existed)
			}
		} else {
			etag, err = put(c, key, b)
		}
	case "del":
		key, s := pickKey(r, "obj", "")
		if key == "" {
			return runOp("put")
		}
		defer unlock(key)
		before, existed = s, true
		o = op{"del", key, s.seq + 1, "-", 0}
		begin(o)
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		_, err = c.DeleteObject(ctx, &s3.DeleteObjectInput{Bucket: aws.String(bucket), Key: aws.String(key)})
		cancel()
	case "mdel":
		return runMdel(r, c, writer)
	case "abort":
		key := fmt.Sprintf("k%da", cmd)
		lock(key)
		defer unlock(key)
		o = op{"abort", key, 0, "-", 0}
		begin(o)
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		cr, e := c.CreateMultipartUpload(ctx, &s3.CreateMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key)})
		if e == nil {
			b := body(tag, 500+r.IntN(20000))
			_, e = c.UploadPart(ctx, &s3.UploadPartInput{Bucket: aws.String(bucket), Key: aws.String(key),
				UploadId: cr.UploadId, PartNumber: aws.Int32(1), Body: bytes.NewReader(b)})
			_, e2 := c.AbortMultipartUpload(ctx, &s3.AbortMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key), UploadId: cr.UploadId})
			if e2 != nil {
				e = e2
			}
		}
		err = e
		// an abort never writes the object, whatever its answer: an error is "failed" for the
		// key's value, and the read-back judges that the key stays absent
		if err != nil {
			appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" failed")
			hrec(cmd, "fail", "send", "objects", o.id())
			fmt.Fprintf(os.Stderr, "gac: %s: %s\n", o.id(), errText(err))
			return 1
		}
	case "copy":
		src, s := pickKey(r, "obj", "")
		if src == "" {
			return runOp("put")
		}
		defer unlock(src)
		dst := ""
		if r.IntN(2) == 0 {
			dst, before = pickKey(r, "", src)
			existed = dst != ""
		}
		if dst == "" {
			dst = fmt.Sprintf("k%d", cmd)
			lock(dst)
		}
		defer unlock(dst)
		o = op{"copy", dst, before.seq + 1, s.val, 0}
		if !existed {
			o.seq = 0
		}
		begin(o)
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		out, e := c.CopyObject(ctx, &s3.CopyObjectInput{Bucket: aws.String(bucket), Key: aws.String(dst),
			CopySource: aws.String(bucket + "/" + src)})
		cancel()
		err = e
		if e == nil && out.CopyObjectResult != nil {
			etag = aws.ToString(out.CopyObjectResult.ETag)
		}
	default:
		fmt.Fprintln(os.Stderr, "gac: unknown operation", kind)
		return 2
	}

	if finish(o, etag, err, before, existed) != "ack" {
		return 1
	}
	if kind == "abort" {
		retire(o.key) // the key of an aborted upload takes no other operation
	}
	if v := readAfterWrite(o, writer, r); v != "" {
		console("gac: " + v)
		fmt.Println(v)
		return 100
	}
	return 0
}

// finishNotDone records a multipart upload that stopped before its completion: failed.
func finishNotDone(o op, err error, before keyState, existed bool) int {
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" failed")
	hrec(o.proc(), "fail", "send", "objects", o.id())
	if existed {
		saveState(o.key, keyState{o.seq, before.val})
	}
	fmt.Fprintf(os.Stderr, "gac: %s: before the completion: %s\n", o.id(), errText(err))
	return 1
}

// runMdel deletes two existing keys with one DeleteObjects request.
func runMdel(r *rand.Rand, c *s3.Client, writer string) int {
	k1, s1 := pickKey(r, "obj", "")
	if k1 == "" {
		return runOp("put")
	}
	defer unlock(k1)
	k2, s2 := pickKey(r, "obj", k1)
	keys := []string{k1}
	states := []keyState{s1}
	if k2 != "" {
		defer unlock(k2)
		keys, states = append(keys, k2), append(states, s2)
	}
	ops := make([]op, len(keys))
	var objs []types.ObjectIdentifier
	for i, k := range keys {
		ops[i] = op{"mdel", k, states[i].seq + 1, "-", i}
		begin(ops[i])
		objs = append(objs, types.ObjectIdentifier{Key: aws.String(k)})
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	out, err := c.DeleteObjects(ctx, &s3.DeleteObjectsInput{Bucket: aws.String(bucket), Delete: &types.Delete{Objects: objs}})
	cancel()
	rc := 0
	for i, o := range ops {
		e := err
		if err == nil {
			e = errors.New("not in the answer") // unknown, unless Errors names it
			for _, d := range out.Deleted {
				if aws.ToString(d.Key) == o.key {
					e = nil
				}
			}
			for _, x := range out.Errors {
				if aws.ToString(x.Key) == o.key {
					e = fmt.Errorf("%s %s", aws.ToString(x.Code), aws.ToString(x.Message))
				}
			}
		}
		if finish(o, "", e, states[i], true) != "ack" {
			rc = 1
			continue
		}
		if v := readAfterWrite(o, writer, r); v != "" {
			console("gac: " + v)
			fmt.Println(v)
			return 100
		}
	}
	return rc
}

// ---- the anytime reader ----

// readSome reads up to 5 known keys through random nodes. A body that is not whole is a violation.
func readSome() int {
	r := rng()
	ents, _ := os.ReadDir(filepath.Join(stateDir, "keys"))
	var names []string
	for _, e := range ents {
		if !strings.HasPrefix(e.Name(), ".") {
			names = append(names, e.Name())
		}
	}
	sort.Strings(names)
	r.Shuffle(len(names), func(i, j int) { names[i], names[j] = names[j], names[i] })
	for _, k := range names[:min(5, len(names))] {
		n := nodes[r.IntN(len(nodes))]
		v, err := readKey(n, k, 10*time.Second)
		if err == nil && v == "corrupt" {
			msg := fmt.Sprintf("a read of %s through %s returned a body that is not whole", k, n)
			console("gac: " + msg)
			fmt.Println(msg)
			return 100
		}
	}
	return 0
}

// ---- the read-back and the checks ----

// opsByKey collects every operation of the run from $HYDO_OUTCOMES (the last outcome of each id)
// and $HYDO_ACKED, grouped by key and sorted by seq.
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

// candidates is the set of values a key may have now: the value of its last acknowledged
// operation (or none, "-", without one) and the value of an operation of unknown outcome after it.
func candidates(ops []op, oc map[string]string) (string, map[string]bool) {
	last := "-"
	for _, o := range ops {
		if oc[o.id()] == "ack" {
			last = o.val
		}
	}
	c := map[string]bool{last: true}
	lastAck := -1
	for i, o := range ops {
		if oc[o.id()] == "ack" {
			lastAck = i
		}
	}
	for i, o := range ops {
		if i > lastAck && oc[o.id()] == "unknown" {
			c[o.val] = true
		}
	}
	return last, c
}

// readAll reads every key until each one has a certain answer or the deadline passes.
func readAll(keys []string, deadline time.Time) map[string]string {
	got := map[string]string{}
	for i := 0; ; i++ {
		left := 0
		for _, k := range keys {
			if _, ok := got[k]; ok {
				continue
			}
			v, err := readKey(nodes[(i+len(got))%len(nodes)], k, 10*time.Second)
			if err == nil {
				got[k] = v
			} else {
				left++
			}
			if time.Now().After(deadline) {
				return got
			}
		}
		if left == 0 || time.Now().After(deadline) {
			return got
		}
		time.Sleep(2 * time.Second)
	}
}

// readBack is the final: of the messages check objects.
func readBack() int {
	const proc = 100000
	by, oc := opsByKey()
	var keys []string
	for k := range by {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	got := readAll(keys, time.Now().Add(150*time.Second))
	var ids, items, unread []string
	for _, k := range keys {
		ops := by[k]
		v, read := got[k]
		if !read {
			for _, o := range ops {
				unread = append(unread, o.id())
			}
			continue
		}
		last, cand := candidates(ops, oc)
		lastAck := ""
		for _, o := range ops {
			if oc[o.id()] == "ack" {
				lastAck = o.id()
			}
		}
		for _, o := range ops {
			it := o.id()
			switch oc[o.id()] {
			case "ack":
				it += ";s=ok"
				if o.id() == lastAck {
					if v == last {
						it += ";f=1"
					} else if !cand[v] {
						it += ";f=0;e=" + enc("the key holds "+v)
					}
				}
			case "failed":
				it += ";s=fail;e=" + enc("refused")
				if v == o.val && !cand[v] && o.val != "-" {
					it += ";f=1"
				}
			default:
				it += ";s=info"
				if v == o.val {
					it += ";f=1"
				} else {
					it += ";f=0"
				}
			}
			if v == "corrupt" && o.id() == lastAck {
				it = o.id() + ";s=ok;f=0;e=" + enc("the body is not whole")
			}
			ids, items = append(ids, o.id()), append(items, it)
		}
		if !cand[v] {
			fmt.Printf("%s holds %s, which no acknowledged or unknown write left there (expected %s)\n", k, v, strings.Join(setList(cand), " or "))
		}
	}
	for i := 0; i < len(items); i += 20 {
		j := min(i+20, len(items))
		hrec(proc, "invoke", append([]string{"status", "objects"}, ids[i:j]...)...)
		hrec(proc, "ok", append([]string{"status", "objects"}, items[i:j]...)...)
	}
	for i := 0; i < len(unread); i += 20 {
		j := min(i+20, len(unread))
		hrec(proc, "invoke", append([]string{"status", "objects"}, unread[i:j]...)...)
		hrec(proc, "info", append([]string{"status", "objects"}, unread[i:j]...)...)
	}
	fmt.Printf("read back %d keys of %d (%d operations)\n", len(got), len(keys), len(items))
	return 0
}

func setList(m map[string]bool) []string {
	var out []string
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

// checkList compares ListObjectsV2 with the keys' values: every key that exists is listed, and
// no key that is absent is listed, nor a key that no operation wrote.
func checkList() int {
	by, _ := opsByKey()
	var keys []string
	for k := range by {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	listed := map[string]bool{}
	var lerr error
	for deadline := time.Now().Add(25 * time.Second); ; {
		listed, lerr = listAll(nodes[0])
		if lerr == nil || time.Now().After(deadline) {
			break
		}
		time.Sleep(2 * time.Second)
	}
	if lerr != nil {
		fmt.Println("cannot list the bucket:", errText(lerr))
		return 2
	}
	fmt.Printf("listed through %s: %d keys\n", nodes[0], len(listed))
	got := readAll(keys, time.Now().Add(25*time.Second))
	bad := 0
	for _, k := range keys {
		v, read := got[k]
		switch {
		case read && v != "-" && !listed[k]:
			fmt.Printf("%s exists (%s) and is not listed\n", k, v)
			bad++
		case read && v == "-" && listed[k]:
			fmt.Printf("%s does not exist and is listed\n", k)
			bad++
		}
	}
	for k := range listed {
		if _, ok := by[k]; !ok && !strings.HasPrefix(k, "init-") { // first_init's probes
			fmt.Printf("%s is listed and no operation wrote it\n", k)
			bad++
		}
	}
	if bad > 0 {
		fmt.Printf("the listing differs from the objects in %d keys\n", bad)
		return 1
	}
	fmt.Printf("the listing of %d keys matches the objects\n", len(listed))
	return 0
}

func listAll(node string) (map[string]bool, error) {
	c := client(node, 20*time.Second)
	out := map[string]bool{}
	var tok *string
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		res, err := c.ListObjectsV2(ctx, &s3.ListObjectsV2Input{Bucket: aws.String(bucket), ContinuationToken: tok})
		cancel()
		if err != nil {
			return nil, err
		}
		for _, o := range res.Contents {
			out[aws.ToString(o.Key)] = true
		}
		if !aws.ToBool(res.IsTruncated) {
			return out, nil
		}
		tok = res.NextContinuationToken
	}
}

// checkUploads: no upload of a key whose abort Garage acknowledged is still listed.
func checkUploads() int {
	by, oc := opsByKey()
	var res *s3.ListMultipartUploadsOutput
	var err error
	for i, deadline := 0, time.Now().Add(25*time.Second); ; i++ {
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		res, err = client(nodes[i%len(nodes)], 20*time.Second).ListMultipartUploads(ctx, &s3.ListMultipartUploadsInput{Bucket: aws.String(bucket)})
		cancel()
		if err == nil || time.Now().After(deadline) {
			break
		}
		time.Sleep(2 * time.Second)
	}
	if err != nil {
		fmt.Println("cannot list the uploads:", errText(err))
		return 2
	}
	bad := 0
	for _, u := range res.Uploads {
		k := aws.ToString(u.Key)
		for _, o := range by[k] {
			if o.kind == "abort" && oc[o.id()] == "ack" {
				fmt.Printf("the upload %s of %s is listed after its abort was acknowledged\n", aws.ToString(u.UploadId), k)
				bad++
			}
		}
	}
	if bad > 0 {
		fmt.Printf("%d aborted uploads are still listed\n", bad)
		return 1
	}
	fmt.Printf("%d uploads listed, none aborted\n", len(res.Uploads))
	return 0
}

// checkAvailable is one poll of eventually_available: a ListObjectsV2 through every node
// succeeds. It prints the node that did not answer.
func checkAvailable() int {
	for _, n := range nodes {
		if _, err := listAll(n); err != nil {
			fmt.Printf("%s: %s\n", n, errText(err))
			fmt.Println("a node does not answer a listing")
			return 1
		}
	}
	fmt.Println("every node answers a listing")
	return 0
}

// ---- the cluster's setup through the admin API ----

func admin(node, method, path string, in, out any) error {
	var rd io.Reader
	if in != nil {
		b, _ := json.Marshal(in)
		rd = bytes.NewReader(b)
	}
	req, _ := http.NewRequest(method, "http://"+node+":3903"+path, rd)
	req.Header.Set("Authorization", "Bearer "+adminToken)
	req.Header.Set("Content-Type", "application/json")
	resp, err := (&http.Client{Timeout: 20 * time.Second}).Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("%s %s: %d %s", method, path, resp.StatusCode, string(b))
	}
	if out != nil {
		return json.Unmarshal(b, out)
	}
	return nil
}

func retry(what string, d time.Duration, f func() error) error {
	deadline := time.Now().Add(d)
	for {
		err := f()
		if err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("%s: %w", what, err)
		}
		time.Sleep(time.Second)
	}
}

func cmdInit() int {
	type nodeResp struct {
		ID   string  `json:"id"`
		Addr *string `json:"addr"`
		IsUp bool    `json:"isUp"`
	}
	type status struct {
		LayoutVersion int        `json:"layoutVersion"`
		Nodes         []nodeResp `json:"nodes"`
	}
	ids := make([]string, len(nodes))
	for i, n := range nodes {
		var st status
		if err := retry("the status of "+n, 60*time.Second, func() error { return admin(n, "GET", "/v2/GetClusterStatus", nil, &st) }); err != nil {
			fmt.Println(err)
			return 1
		}
		for _, x := range st.Nodes {
			if x.IsUp && x.Addr != nil && strings.HasPrefix(*x.Addr, "10.89.0.1"+strconv.Itoa(i+1)+":") {
				ids[i] = x.ID
			}
		}
		if ids[i] == "" && len(st.Nodes) == 1 {
			ids[i] = st.Nodes[0].ID
		}
		if ids[i] == "" {
			fmt.Printf("cannot find the id of %s in %+v\n", n, st.Nodes)
			return 1
		}
	}
	peers := []string{ids[1] + "@10.89.0.12:3901", ids[2] + "@10.89.0.13:3901"}
	if err := retry("connect", 60*time.Second, func() error { return admin(nodes[0], "POST", "/v2/ConnectClusterNodes", peers, nil) }); err != nil {
		fmt.Println(err)
		return 1
	}
	var roles []map[string]any
	for _, id := range ids {
		roles = append(roles, map[string]any{"id": id, "zone": "dc1", "capacity": 1 << 30, "tags": []string{}})
	}
	if err := retry("layout", 60*time.Second, func() error {
		return admin(nodes[0], "POST", "/v2/UpdateClusterLayout", map[string]any{"roles": roles}, nil)
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	if err := retry("apply", 60*time.Second, func() error {
		return admin(nodes[0], "POST", "/v2/ApplyClusterLayout", map[string]any{"version": 1}, nil)
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	// wait until every node has the layout and sees the others
	for _, n := range nodes {
		if err := retry("health of "+n, 120*time.Second, func() error {
			var h struct {
				Status string `json:"status"`
			}
			if err := admin(n, "GET", "/v2/GetClusterHealth", nil, &h); err != nil {
				return err
			}
			if h.Status != "healthy" {
				return fmt.Errorf("%s", h.Status)
			}
			return nil
		}); err != nil {
			fmt.Println(err)
			return 1
		}
	}
	if err := retry("key", 60*time.Second, func() error {
		return admin(nodes[0], "POST", "/v2/ImportKey", map[string]any{"accessKeyId": accessKey, "secretAccessKey": secretKey, "name": "hydo"}, nil)
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	var b struct {
		ID string `json:"id"`
	}
	if err := retry("bucket", 60*time.Second, func() error {
		return admin(nodes[0], "POST", "/v2/CreateBucket", map[string]any{"globalAlias": bucket}, &b)
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	if err := retry("allow", 60*time.Second, func() error {
		return admin(nodes[0], "POST", "/v2/AllowBucketKey", map[string]any{"bucketId": b.ID, "accessKeyId": accessKey,
			"permissions": map[string]bool{"read": true, "write": true, "owner": true}}, nil)
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	// the bucket answers through every node before the workload starts
	for _, n := range nodes {
		if err := retry("a write through "+n, 120*time.Second, func() error {
			_, err := put(client(n, 20*time.Second), "init-"+n, body("init-"+n, 100))
			return err
		}); err != nil {
			fmt.Println(err)
			return 1
		}
	}
	// Garage keeps the addresses of its peers in <metadata_dir>/peer_list, which its discovery
	// loop writes once a minute. Until it lists the two other nodes, a power cut of every node
	// leaves each one without its peers, and the cluster does not form again (RESULTS.md,
	// finding 3). Wait for the lists, so that the explorations' power cuts test the data.
	start := time.Now()
	// hydo build --set peerwait=no leaves the wait out (/etc/gac/peer-wait), for finding 3's regression
	if b, _ := os.ReadFile("/etc/gac/peer-wait"); strings.TrimSpace(string(b)) != "no" {
		for i := range nodes {
			p := fmt.Sprintf("/data/vol/garage%d/meta/peer_list", i+1)
			if err := retry("the peer list of "+nodes[i], 150*time.Second, func() error {
				st, err := os.Stat(p)
				if err != nil {
					return err
				}
				if st.Size() < 90 { // 1 byte for no peer, 97 for the two others

					return fmt.Errorf("%d bytes", st.Size())
				}
				return nil
			}); err != nil {
				fmt.Println(err)
				return 1
			}
		}
	}
	fmt.Printf("the cluster is up: layout 1 with %s; the peer lists took %.0f s\n", strings.Join(ids, ", "), time.Since(start).Seconds())
	return 0
}

func cmdGet(key string) int {
	for _, n := range nodes {
		v, err := readKey(n, key, 10*time.Second)
		if err != nil {
			fmt.Printf("%s: %s\n", n, errText(err))
		} else {
			fmt.Printf("%s: %s\n", n, v)
		}
	}
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: gac init | op <kind> | read | readback | check list|uploads | get <key>")
		os.Exit(2)
	}
	arg := ""
	if len(os.Args) > 2 {
		arg = os.Args[2]
	}
	switch os.Args[1] {
	case "init":
		os.Exit(cmdInit())
	case "op":
		os.Exit(runOp(arg))
	case "read":
		os.Exit(readSome())
	case "readback":
		os.Exit(readBack())
	case "check":
		switch arg {
		case "list":
			os.Exit(checkList())
		case "uploads":
			os.Exit(checkUploads())
		case "available":
			os.Exit(checkAvailable())
		}
	case "get":
		os.Exit(cmdGet(arg))
	case "list":
		// gac list <node>: the keys that ListObjectsV2 through one node lists, and nothing else
		// (no GET, which would repair a stale replica)
		node := nodes[0]
		if arg != "" {
			node = arg
		}
		keys, err := listAll(node)
		if err != nil {
			fmt.Println(errText(err))
			os.Exit(2)
		}
		var ks []string
		for k := range keys {
			ks = append(ks, k)
		}
		sort.Strings(ks)
		fmt.Println(strings.Join(ks, " "))
		os.Exit(0)
	}
	fmt.Fprintln(os.Stderr, "gac: unknown command", os.Args[1:])
	os.Exit(2)
}
