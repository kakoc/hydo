// swc is the test client of the SeaweedFS example: the cluster's setup, the workload's
// operations and the checks. It speaks S3 to the S3 gateway with the AWS SDK for Go v2, and the
// filer's, the masters' and the volume servers' HTTP APIs with Go's standard library.
//
//	swc init              wait for the cluster, set the filer's path rule, make the bucket hydo
//	swc op <kind>         one operation; the composer's HYDO_* variables drive it
//	swc read              read some objects through either API and check that each one is whole
//	swc readback          the final: of the messages check: each operation's effect, read back
//	swc check <rule>      one final check: list, uploads, replicas or available
//	swc get <key>         print what S3, the filer and each replica answer for one key
//
// The services are master1..3 (9333), volume1..3 (8080), filer (8888) and s3 (8333). Every key
// is an object of the bucket hydo, which is the filer's folder /buckets/hydo: an operation writes
// it through S3 or through the filer, and the read after an acknowledged write goes through the
// API that the command's seed picks, often the other one.
//
// Objects. Every body starts with one line, "swc <tag> <n> <sha256>", followed by n bytes that
// the tag's seed makes; the sha256 is that of the n bytes. A read is whole when the line matches
// the bytes after it. The tag v<cmd>x0 is unique per write; a copy carries the tag of its
// source. A key's value is the tag of its body, "-" when the key does not exist, or "corrupt"
// when the body is not whole.
//
// Operations, one id each: <kind>.<key>.<seq>.<value>, where seq counts the operations on the key
// and value is the tag the operation writes ("-" for a deletion and an aborted upload):
//
//	put    S3 PutObject of a new key
//	over   S3 PutObject over an existing key
//	del    S3 DeleteObject of an existing key
//	mdel   S3 DeleteObjects of two existing keys (one id per key)
//	mpu    an S3 multipart upload of 2 or 3 parts, 5 MiB each but the last, to a new or an existing key, then its completion
//	abort  an S3 multipart upload of one part to a new key, then its abort
//	copy   S3 CopyObject of an existing key to a new or an existing key
//	fput   a filer PUT of a new file /buckets/hydo/<key>
//	fover  a filer PUT over an existing key
//	fdel   a filer DELETE of an existing key
//
// One command at a time works on a key (a lock file under /tmp/swc/lock). A key's last known
// value and seq are in /tmp/swc/keys/<key>, for the keys whose every operation got a certain
// answer. A key whose operation got no certain answer is never written again, so each key ends
// with at most one operation of unknown outcome.
//
// Outcomes. A 2xx answer is acknowledged. A 4xx answer but 408 and 429 is failed: SeaweedFS did
// not apply it. Every other answer, and no answer, is unknown. $HYDO_ACKED gets the id of each
// acknowledged operation, $HYDO_OUTCOMES gets "<id> ack|failed|unknown".
//
// The history objects (hydo guide messages): each operation is a send of its id to the channel
// objects. `swc readback` reads every key back and writes the final state of each operation:
// s=ok for acknowledged, s=fail for failed, s=info for unknown, and f=1 or f=0 for whether the
// operation's value is the key's value now, wherever that is decided.
package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"net/url"
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
	bucket    = "hydo"
	accessKey = "hydoaccesskey0000001"
	secretKey = "hydosecretkey000000000000000000000000001"
	stateDir  = "/tmp/swc"
	s3URL     = "http://s3:8333"
	filerURL  = "http://filer:8888"
)

var (
	masters = []string{"master1", "master2", "master3"}
	volumes = []string{"volume1", "volume2", "volume3"}
)

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

// event writes an SDK assertion record: the event of a reaction (A=sw.put_acked:cut).
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

// ---- S3 and HTTP ----

func s3client(timeout time.Duration) *s3.Client {
	return s3.New(s3.Options{
		Region:       "us-east-1",
		BaseEndpoint: aws.String(s3URL),
		UsePathStyle: true,
		Credentials: aws.CredentialsProviderFunc(func(context.Context) (aws.Credentials, error) {
			return aws.Credentials{AccessKeyID: accessKey, SecretAccessKey: secretKey}, nil
		}),
		Retryer:    aws.NopRetryer{},
		HTTPClient: &http.Client{Timeout: timeout},
	})
}

// httpError is an HTTP answer that is not 2xx.
type httpError struct {
	code int
	msg  string
}

func (e httpError) Error() string { return fmt.Sprintf("%d %s", e.code, e.msg) }

// status is the HTTP status of an error, or 0 when no answer came.
func status(err error) int {
	var re *awshttp.ResponseError
	if errors.As(err, &re) {
		return re.HTTPStatusCode()
	}
	var he httpError
	if errors.As(err, &he) {
		return he.code
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
		var he httpError
		if errors.As(err, &he) {
			return he.Error()
		}
		return fmt.Sprintf("%d", st)
	}
	s := err.Error()
	if len(s) > 160 {
		s = s[len(s)-160:]
	}
	return "no answer: " + s
}

// do sends one HTTP request and returns the body of a 2xx answer, or an httpError.
func do(method, u string, body []byte, timeout time.Duration) ([]byte, error) {
	var rd io.Reader
	if body != nil {
		rd = bytes.NewReader(body)
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, method, u, rd)
	if err != nil {
		return nil, err
	}
	if body != nil {
		req.ContentLength = int64(len(body))
		req.Header.Set("Content-Type", "application/octet-stream")
	}
	resp, err := (&http.Client{Timeout: timeout}).Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		m := strings.TrimSpace(string(b))
		if len(m) > 120 {
			m = m[:120]
		}
		return nil, httpError{resp.StatusCode, m}
	}
	return b, err
}

func filerPath(key string) string { return filerURL + "/buckets/" + bucket + "/" + key }

// body makes the body of tag: the header line and n bytes from the tag's seed.
func body(tag string, n int) []byte {
	h := sha256.Sum256([]byte(tag))
	r := rand.New(rand.NewPCG(binary.BigEndian.Uint64(h[0:8]), binary.BigEndian.Uint64(h[8:16])))
	data := make([]byte, n)
	for i := range data {
		data[i] = byte('a' + r.IntN(26))
	}
	sum := sha256.Sum256(data)
	return append([]byte(fmt.Sprintf("swc %s %d %s\n", tag, n, hex.EncodeToString(sum[:]))), data...)
}

// valueOf reads a body: its tag when it is whole, "corrupt" otherwise.
func valueOf(b []byte) string {
	i := bytes.IndexByte(b, '\n')
	if i < 0 {
		return "corrupt"
	}
	f := strings.Fields(string(b[:i]))
	if len(f) != 4 || f[0] != "swc" {
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

// size draws an object size: most small, some of tens of KiB, a few over 1 MiB, and one in 20
// over the filer's chunk size of 4 MiB (two chunks).
func size(r *rand.Rand) int {
	switch x := r.IntN(20); {
	case x < 9:
		return 64 + r.IntN(2000)
	case x < 17:
		return 8192 + r.IntN(120000)
	case x < 19:
		return 1100000 + r.IntN(300000)
	default:
		return 4300000 + r.IntN(300000)
	}
}

// readS3 reads key through S3: its value ("-" for none), or an error when no certain answer came.
func readS3(key string, timeout time.Duration) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	out, err := s3client(timeout).GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(bucket), Key: aws.String(key)})
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

// readFiler reads key through the filer.
func readFiler(key string, timeout time.Duration) (string, error) {
	b, err := do("GET", filerPath(key), nil, timeout)
	if err != nil {
		if status(err) == 404 {
			return "-", nil
		}
		return "", err
	}
	return valueOf(b), nil
}

func readKey(api, key string, timeout time.Duration) (string, error) {
	if api == "filer" {
		return readFiler(key, timeout)
	}
	return readS3(key, timeout)
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
func finish(o op, err error, before keyState, existed bool) string {
	oc := "ack"
	if err != nil {
		oc = outcome(err)
	}
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" "+oc)
	switch oc {
	case "ack":
		appendLine(os.Getenv("HYDO_ACKED"), o.id())
		hrec(o.proc(), "ok", "send", "objects", o.id())
		saveState(o.key, keyState{o.seq, o.val})
		event("sw."+o.kind+"_acked", "SeaweedFS acknowledged a "+o.kind)
	case "failed":
		hrec(o.proc(), "fail", "send", "objects", o.id())
		if existed {
			saveState(o.key, keyState{o.seq, before.val})
		}
		fmt.Fprintf(os.Stderr, "swc: %s: %s\n", o.id(), errText(err))
	default:
		hrec(o.proc(), "info", "send", "objects", o.id())
		retire(o.key)
		fmt.Fprintf(os.Stderr, "swc: %s: %s\n", o.id(), errText(err))
	}
	return oc
}

// readAfterWrite reads the key through an API the seed picks, until a certain answer or 3
// tries. A certain answer other than the written value breaks read-after-write: it returns a
// description then.
func readAfterWrite(o op, via string, r *rand.Rand) string {
	api := "s3"
	if r.IntN(3) > 0 { // two in three reads go through the other API
		if via == "s3" {
			api = "filer"
		}
	} else if via == "filer" {
		api = "filer"
	}
	for try := 0; try < 3; try++ {
		v, err := readKey(api, o.key, 10*time.Second)
		if err != nil {
			time.Sleep(time.Second)
			continue
		}
		if v != o.val {
			return fmt.Sprintf("read-after-write: %s was acknowledged through %s, and then %s answered %s for %s", o.id(), via, api, v, o.key)
		}
		return ""
	}
	return ""
}

func put(c *s3.Client, key string, b []byte) error {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	_, err := c.PutObject(ctx, &s3.PutObjectInput{Bucket: aws.String(bucket), Key: aws.String(key), Body: bytes.NewReader(b)})
	return err
}

func multipart(c *s3.Client, key string, b []byte, nparts int) error {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	cr, err := c.CreateMultipartUpload(ctx, &s3.CreateMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key)})
	if err != nil {
		return errNotDone{err}
	}
	var parts []types.CompletedPart
	// every part but the last has the minimum size of 5 MiB (S3's rule, which SeaweedFS checks at
	// the completion: EntityTooSmall)
	const step = 5 << 20
	for i := 0; i < nparts; i++ {
		lo, hi := i*step, min((i+1)*step, len(b))
		if i == nparts-1 {
			hi = len(b)
		}
		up, err := c.UploadPart(ctx, &s3.UploadPartInput{Bucket: aws.String(bucket), Key: aws.String(key),
			UploadId: cr.UploadId, PartNumber: aws.Int32(int32(i + 1)), Body: bytes.NewReader(b[lo:hi])})
		if err != nil {
			c.AbortMultipartUpload(context.Background(), &s3.AbortMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key), UploadId: cr.UploadId})
			return errNotDone{err}
		}
		parts = append(parts, types.CompletedPart{ETag: up.ETag, PartNumber: aws.Int32(int32(i + 1))})
	}
	_, err = c.CompleteMultipartUpload(ctx, &s3.CompleteMultipartUploadInput{Bucket: aws.String(bucket), Key: aws.String(key),
		UploadId: cr.UploadId, MultipartUpload: &types.CompletedMultipartUpload{Parts: parts}})
	return err
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
	c := s3client(30 * time.Second)
	via := "s3"

	var o op
	var before keyState
	existed := false
	var err error

	switch kind {
	case "put", "over", "mpu", "fput", "fover":
		key := ""
		if kind == "over" || kind == "fover" || (kind == "mpu" && r.IntN(2) == 0) {
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
		nparts := 2 + r.IntN(2)
		if kind == "mpu" {
			b = body(tag, (nparts-1)*(5<<20)+64+r.IntN(200000))
		}
		begin(o)
		if len(b) >= 4<<20 {
			// a write of more than one chunk: the event of the kills inside a large write
			event("sw.big_start", "a write of 4 MiB or more starts")
		}
		switch kind {
		case "mpu":
			err = multipart(c, key, b, nparts)
			var nd errNotDone
			if errors.As(err, &nd) {
				// a step before the completion failed: the object was not written
				return finishNotDone(o, nd.err, before, existed)
			}
		case "fput", "fover":
			via = "filer"
			_, err = do("PUT", filerPath(key), b, 30*time.Second)
		default:
			err = put(c, key, b)
		}
	case "del", "fdel":
		key, s := pickKey(r, "obj", "")
		if key == "" {
			return runOp("put")
		}
		defer unlock(key)
		before, existed = s, true
		o = op{kind, key, s.seq + 1, "-", 0}
		begin(o)
		if kind == "fdel" {
			via = "filer"
			_, err = do("DELETE", filerPath(key), nil, 30*time.Second)
		} else {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			_, err = c.DeleteObject(ctx, &s3.DeleteObjectInput{Bucket: aws.String(bucket), Key: aws.String(key)})
			cancel()
		}
	case "mdel":
		return runMdel(r, c)
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
			if e == nil {
				appendLine(filepath.Join(stateDir, "aborted"), key+" "+aws.ToString(cr.UploadId))
			}
		}
		err = e
		// an abort never writes the object, whatever its answer: an error is "failed" for the
		// key's value, and the read-back judges that the key stays absent
		if err != nil {
			appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" failed")
			hrec(cmd, "fail", "send", "objects", o.id())
			fmt.Fprintf(os.Stderr, "swc: %s: %s\n", o.id(), errText(err))
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
		_, err = c.CopyObject(ctx, &s3.CopyObjectInput{Bucket: aws.String(bucket), Key: aws.String(dst),
			CopySource: aws.String(bucket + "/" + src)})
		cancel()
	default:
		fmt.Fprintln(os.Stderr, "swc: unknown operation", kind)
		return 2
	}

	if finish(o, err, before, existed) != "ack" {
		return 1
	}
	if kind == "abort" {
		retire(o.key) // the key of an aborted upload takes no other operation
	}
	if v := readAfterWrite(o, via, r); v != "" {
		console("swc: " + v)
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
	fmt.Fprintf(os.Stderr, "swc: %s: before the completion: %s\n", o.id(), errText(err))
	return 1
}

// runMdel deletes two existing keys with one DeleteObjects request.
func runMdel(r *rand.Rand, c *s3.Client) int {
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
		if finish(o, e, states[i], true) != "ack" {
			rc = 1
			continue
		}
		if v := readAfterWrite(o, "s3", r); v != "" {
			console("swc: " + v)
			fmt.Println(v)
			return 100
		}
	}
	return rc
}

// ---- the anytime reader ----

// readSome reads up to 5 known keys through either API. A body that is not whole is a violation.
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
		api := []string{"s3", "filer"}[r.IntN(2)]
		v, err := readKey(api, k, 10*time.Second)
		if err == nil && v == "corrupt" {
			msg := fmt.Sprintf("a read of %s through %s returned a body that is not whole", k, api)
			console("swc: " + msg)
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
	lastAck := -1
	for i, o := range ops {
		if oc[o.id()] == "ack" {
			last, lastAck = o.val, i
		}
	}
	c := map[string]bool{last: true}
	for i, o := range ops {
		if i > lastAck && oc[o.id()] == "unknown" {
			c[o.val] = true
		}
	}
	return last, c
}

// readAll reads every key through S3 until each one has a certain answer or the deadline passes.
func readAll(keys []string, deadline time.Time) map[string]string {
	got := map[string]string{}
	for {
		left := 0
		for _, k := range keys {
			if _, ok := got[k]; ok {
				continue
			}
			v, err := readS3(k, 10*time.Second)
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

// vacuum compacts every volume that holds a deleted needle, through any master (it answers
// from the leader), as the masters' own vacuum does every 14 to 16 minutes with garbageThreshold 0.3.
// The filer reads chunks with readDeleted=true, so a chunk that SeaweedFS deleted while an entry
// still names it stays readable until its volume is compacted; after the vacuum it is gone.
func vacuum() {
	var last error
	for try := 0; try < 6; try++ {
		for _, m := range masters {
			_, err := do("GET", "http://"+m+":9333/vol/vacuum?garbageThreshold=0.0001", nil, 120*time.Second)
			if err == nil {
				fmt.Printf("vacuum: compacted through %s\n", m)
				return
			}
			last = err
		}
		time.Sleep(5 * time.Second)
	}
	fmt.Printf("vacuum: no master compacted the volumes: %s\n", errText(last))
}

// runVacuum is the anytime_vacuum command: one compaction of every volume with a deleted needle,
// concurrent with the writes, between the console events sw.vacuum_start and sw.vacuum_done.
func runVacuum() int {
	event("sw.vacuum_start", "a vacuum of the volumes starts")
	for _, m := range masters {
		if _, err := do("GET", "http://"+m+":9333/vol/vacuum?garbageThreshold=0.0001", nil, 90*time.Second); err == nil {
			event("sw.vacuum_done", "a vacuum of the volumes ended")
			return 0
		} else {
			fmt.Fprintf(os.Stderr, "swc: vacuum through %s: %s\n", m, errText(err))
		}
	}
	return 1
}

// readBack is the final: of the messages check objects. With the build parameter vacuum=yes
// (/etc/swc/vacuum) it compacts the volumes first.
func readBack() int {
	const proc = 100000
	if b, _ := os.ReadFile("/etc/swc/vacuum"); strings.TrimSpace(string(b)) == "yes" {
		vacuum()
	}
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
	var listed map[string]bool
	var lerr error
	for deadline := time.Now().Add(25 * time.Second); ; {
		listed, lerr = listAll()
		if lerr == nil || time.Now().After(deadline) {
			break
		}
		time.Sleep(2 * time.Second)
	}
	if lerr != nil {
		fmt.Println("cannot list the bucket:", errText(lerr))
		return 2
	}
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

func listAll() (map[string]bool, error) {
	c := s3client(20 * time.Second)
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

// checkUploads: no upload whose abort SeaweedFS acknowledged is still listed.
func checkUploads() int {
	by, oc := opsByKey()
	var res *s3.ListMultipartUploadsOutput
	var err error
	for deadline := time.Now().Add(25 * time.Second); ; {
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		res, err = s3client(20*time.Second).ListMultipartUploads(ctx, &s3.ListMultipartUploadsInput{Bucket: aws.String(bucket)})
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

// ---- the replicas of each chunk ----

type fidJSON struct {
	VolumeID uint32 `json:"volume_id"`
	FileKey  uint64 `json:"file_key"`
	Cookie   uint32 `json:"cookie"`
}

type chunkJSON struct {
	FileID string   `json:"file_id"`
	Fid    *fidJSON `json:"fid"`
	Offset int64    `json:"offset"`
	Size   uint64   `json:"size"`
}

// fid is a chunk's file id as the volume servers name it: <volume>,<key><cookie> in hex, with
// the key's leading zero bytes left out.
func (c chunkJSON) fid() string {
	if c.FileID != "" {
		return c.FileID
	}
	if c.Fid == nil {
		return ""
	}
	b := make([]byte, 12)
	binary.BigEndian.PutUint64(b[0:8], c.Fid.FileKey)
	binary.BigEndian.PutUint32(b[8:12], c.Fid.Cookie)
	i := 0
	for i < 8 && b[i] == 0 {
		i++
	}
	return fmt.Sprintf("%d,%s", c.Fid.VolumeID, hex.EncodeToString(b[i:]))
}

func chunksOf(key string) ([]chunkJSON, error) {
	b, err := do("GET", filerPath(key)+"?metadata=true&resolveManifest=true", nil, 20*time.Second)
	if err != nil {
		return nil, err
	}
	var e struct {
		Chunks []chunkJSON `json:"chunks"`
	}
	if err := json.Unmarshal(b, &e); err != nil {
		return nil, err
	}
	return e.Chunks, nil
}

// locations asks the masters where a volume is: the volume servers' addresses.
func locations(vid string) ([]string, error) {
	var last error
	for _, m := range masters {
		b, err := do("GET", "http://"+m+":9333/dir/lookup?volumeId="+url.QueryEscape(vid), nil, 10*time.Second)
		if err != nil {
			last = err
			continue
		}
		var res struct {
			Locations []struct {
				URL string `json:"url"`
			} `json:"locations"`
			Error string `json:"error"`
		}
		if err := json.Unmarshal(b, &res); err != nil {
			last = err
			continue
		}
		if res.Error != "" {
			last = errors.New(res.Error)
			continue
		}
		var out []string
		for _, l := range res.Locations {
			out = append(out, l.URL)
		}
		sort.Strings(out)
		return out, nil
	}
	return nil, last
}

// checkReplicas: every chunk of every object that S3 reads whole has two copies (the
// replication 001), registered with the masters, and both answer the chunk with the same bytes.
func checkReplicas() int {
	by, _ := opsByKey()
	var keys []string
	for k := range by {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	got := readAll(keys, time.Now().Add(30*time.Second))
	bad, chunks := 0, 0
	for _, k := range keys {
		if v := got[k]; v == "" || v == "-" || v == "corrupt" {
			continue
		}
		cs, err := chunksOf(k)
		if err != nil {
			fmt.Printf("%s: cannot read its metadata: %s\n", k, errText(err))
			continue
		}
		for _, c := range cs {
			f := c.fid()
			if f == "" {
				continue
			}
			chunks++
			vid := f[:strings.IndexByte(f, ',')]
			var locs []string
			for deadline := time.Now().Add(30 * time.Second); ; {
				locs, err = locations(vid)
				if (err == nil && len(locs) >= 2) || time.Now().After(deadline) {
					break
				}
				time.Sleep(2 * time.Second)
			}
			if err != nil {
				fmt.Printf("%s: chunk %s: no master names its volume: %s\n", k, f, errText(err))
				bad++
				continue
			}
			if len(locs) < 2 {
				fmt.Printf("%s: chunk %s: its volume %s has %d copies (%s), not 2\n", k, f, vid, len(locs), strings.Join(locs, " "))
				bad++
				continue
			}
			var first []byte
			for i, l := range locs {
				b, err := do("GET", "http://"+l+"/"+f, nil, 20*time.Second)
				if err != nil {
					fmt.Printf("%s: chunk %s: %s answers %s\n", k, f, l, errText(err))
					bad++
					break
				}
				if i == 0 {
					first = b
				} else if !bytes.Equal(first, b) {
					fmt.Printf("%s: chunk %s: %s and %s hold different bytes (%d and %d)\n", k, f, locs[0], l, len(first), len(b))
					bad++
				}
			}
		}
	}
	if bad > 0 {
		fmt.Printf("%d chunks of %d lack a whole second copy\n", bad, chunks)
		return 1
	}
	fmt.Printf("every one of %d chunks has its two copies\n", chunks)
	return 0
}

// checkAvailable is one poll of eventually_available: a listing through S3 and through the
// filer succeeds, and every volume server answers its status.
func checkAvailable() int {
	if _, err := listAll(); err != nil {
		fmt.Printf("s3: %s\n", errText(err))
		fmt.Println("the S3 gateway does not answer a listing")
		return 1
	}
	if _, err := do("GET", filerURL+"/buckets/"+bucket+"/?limit=1", nil, 10*time.Second); err != nil {
		fmt.Printf("filer: %s\n", errText(err))
		fmt.Println("the filer does not answer a listing")
		return 1
	}
	for _, v := range volumes {
		if _, err := do("GET", "http://"+v+":8080/status", nil, 10*time.Second); err != nil {
			fmt.Printf("%s: %s\n", v, errText(err))
			fmt.Println("a volume server does not answer")
			return 1
		}
	}
	fmt.Println("S3, the filer and every volume server answer")
	return 0
}

// ---- the cluster's setup ----

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

// leader asks a master for the cluster's status: the leader and the volume servers it knows.
func clusterStatus() (string, int, error) {
	var last error
	for _, m := range masters {
		b, err := do("GET", "http://"+m+":9333/cluster/status", nil, 5*time.Second)
		if err != nil {
			last = err
			continue
		}
		var st struct {
			IsLeader bool   `json:"IsLeader"`
			Leader   string `json:"Leader"`
		}
		if err := json.Unmarshal(b, &st); err != nil || st.Leader == "" {
			last = fmt.Errorf("%s names no leader", m)
			continue
		}
		b, err = do("GET", "http://"+m+":9333/vol/status", nil, 5*time.Second)
		if err != nil {
			last = err
			continue
		}
		n := strings.Count(string(b), `"Url":`)
		return st.Leader, n, nil
	}
	return "", 0, last
}

func cmdInit() int {
	var leader string
	if err := retry("a leader and three volume servers", 180*time.Second, func() error {
		l, _, err := clusterStatus()
		if err != nil {
			return err
		}
		leader = l
		for _, v := range volumes {
			if _, err := do("GET", "http://"+v+":8080/status", nil, 5*time.Second); err != nil {
				return err
			}
		}
		return nil
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	// the filer's path rule: fsync on every write under /buckets/ (wiki "Path Specific
	// Configuration"); /etc/swc/fsync is the build parameter fsync
	fsync := "true"
	if b, err := os.ReadFile("/etc/swc/fsync"); err == nil {
		fsync = strings.TrimSpace(string(b))
	}
	conf := fmt.Sprintf(`{"version":2,"locations":[{"locationPrefix":"/buckets/","fsync":%s}]}`, fsync)
	if err := retry("the filer's path rule", 120*time.Second, func() error {
		_, err := do("PUT", filerURL+"/etc/seaweedfs/filer.conf?saveInside=true", []byte(conf), 10*time.Second)
		return err
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	if err := retry("the bucket", 120*time.Second, func() error {
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		defer cancel()
		_, err := s3client(20*time.Second).CreateBucket(ctx, &s3.CreateBucketInput{Bucket: aws.String(bucket)})
		if err != nil && strings.Contains(err.Error(), "BucketAlreadyOwnedByYou") {
			return nil
		}
		return err
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	// a write through each API answers before the workload starts; the filer applies a changed
	// filer.conf when its metadata subscription sees it, so wait until it reads the rule back
	if err := retry("the rule read back", 60*time.Second, func() error {
		b, err := do("GET", filerURL+"/etc/seaweedfs/filer.conf", nil, 10*time.Second)
		if err != nil {
			return err
		}
		if !strings.Contains(string(b), `"fsync"`) && fsync == "true" {
			return fmt.Errorf("filer.conf has no fsync: %s", string(b))
		}
		return nil
	}); err != nil {
		fmt.Println(err)
		return 1
	}
	// the read above answers from the store; the filer applies the rule when its own metadata
	// subscription delivers the change (maybeReloadFilerConfiguration), within milliseconds
	time.Sleep(3 * time.Second)
	for _, api := range []string{"s3", "filer"} {
		k := "init-" + api
		if err := retry("a write through "+api, 120*time.Second, func() error {
			if api == "s3" {
				return put(s3client(20*time.Second), k, body(k, 100))
			}
			_, err := do("PUT", filerPath(k), body(k, 100), 20*time.Second)
			return err
		}); err != nil {
			fmt.Println(err)
			return 1
		}
	}
	_, n, _ := clusterStatus()
	fmt.Printf("the cluster is up: leader %s, %d volume locations, fsync %s\n", leader, n, fsync)
	return 0
}

func cmdGet(key string) int {
	for _, api := range []string{"s3", "filer"} {
		v, err := readKey(api, key, 10*time.Second)
		if err != nil {
			fmt.Printf("%s: %s\n", api, errText(err))
		} else {
			fmt.Printf("%s: %s\n", api, v)
		}
	}
	cs, err := chunksOf(key)
	if err != nil {
		fmt.Printf("metadata: %s\n", errText(err))
		return 0
	}
	for _, c := range cs {
		f := c.fid()
		locs, err := locations(f[:strings.IndexByte(f, ',')])
		fmt.Printf("chunk %s offset %d size %d: %v %v\n", f, c.Offset, c.Size, locs, err)
		for _, v := range volumes {
			b, err := do("GET", "http://"+v+":8080/"+f, nil, 10*time.Second)
			if err != nil {
				fmt.Printf("  %s: %s\n", v, errText(err))
			} else {
				fmt.Printf("  %s: %d bytes\n", v, len(b))
			}
		}
	}
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: swc init | op <kind> | read | readback | check list|uploads|replicas|available | get <key>")
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
	case "vacuum":
		os.Exit(runVacuum())
	case "readback":
		os.Exit(readBack())
	case "check":
		switch arg {
		case "list":
			os.Exit(checkList())
		case "uploads":
			os.Exit(checkUploads())
		case "replicas":
			os.Exit(checkReplicas())
		case "available":
			os.Exit(checkAvailable())
		}
	case "get":
		os.Exit(cmdGet(arg))
	}
	fmt.Fprintln(os.Stderr, "swc: unknown command", os.Args[1:])
	os.Exit(2)
}
