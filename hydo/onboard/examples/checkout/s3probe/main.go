// s3probe: the request shapes of Milestone 329's s3 proxy fix, sent through the s3 emulator's
// fault proxy in a VM (hydo-s3.yaml next to this folder, Milestone 341).
//
//	s3probe step <n>   one round through the proxy at $AWS_ENDPOINT_URL_S3: a small PUT and its
//	                   HEAD, a 3,000,000-byte PUT read back with HEAD and GET, a PUT with
//	                   If-None-Match: * twice, and PUTs with If-Match (the right ETag, then
//	                   another). Exit 0 when every answer is what S3 gives, or a 412 that the
//	                   proxy gave without reaching the server (an H= rule; the server has no
//	                   such change).
//	s3probe check      every round in $HYDO_ACKED left its objects on the server, read directly
//	s3probe cut-step <n>  one round of hydo-s3-cut.yaml (Milestone 354): a PUT of 64 bytes, a PUT
//	                   of 1 MiB, a copy, a multipart upload of 5 MiB and 1 KiB, and a DELETE of
//	                   the round before's first object. Each acknowledged write is a word in
//	                   $HYDO_ACKED and then the SDK event s3.acked, at which a reaction may cut.
//	s3probe cut-check  every write in $HYDO_ACKED is on the server as acknowledged, read directly
//
// Every request and its answer is one console line ("s3probe <n> <what> ..."). Standard library
// only.
package main

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"
)

// the emulator's own port, behind its proxy at $AWS_ENDPOINT_URL_S3 (as checkout's check)
const direct = "http://127.0.0.1:19000"

const bigSize = 3_000_000

var client = &http.Client{Timeout: 10 * time.Second}

type answer struct {
	status int
	length int64 // the answer's Content-Length; -1 when it had none
	header http.Header
	body   []byte
}

func do(method, url string, body []byte, hdr map[string]string) (answer, error) {
	var rd io.Reader
	if body != nil {
		rd = bytes.NewReader(body)
	}
	req, err := http.NewRequest(method, url, rd)
	if err != nil {
		return answer{}, err
	}
	for k, v := range hdr {
		req.Header.Set(k, v)
	}
	resp, err := client.Do(req)
	if err != nil {
		return answer{}, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return answer{}, err
	}
	a := answer{status: resp.StatusCode, length: -1, header: resp.Header, body: b}
	if cl := resp.Header.Get("Content-Length"); cl != "" {
		a.length, _ = strconv.ParseInt(cl, 10, 64)
	} else if resp.ContentLength >= 0 {
		a.length = resp.ContentLength
	}
	return a, nil
}

func big(n string) []byte {
	b := make([]byte, bigSize)
	seed := byte(len(n) * 7)
	for i := range b {
		b[i] = byte(i*31) ^ seed
	}
	return b
}

func short(b []byte) string {
	s := sha256.Sum256(b)
	return fmt.Sprintf("%x", s[:6])
}

// step runs one round; every mismatch is a line with "unexpected" and makes the exit status 1.
func step(n string) int {
	proxy := os.Getenv("AWS_ENDPOINT_URL_S3")
	if proxy == "" {
		fmt.Println("s3probe: AWS_ENDPOINT_URL_S3 is not set")
		return 2
	}
	bad := 0
	say := func(ok bool, format string, a ...any) {
		w := "ok"
		if !ok {
			w = "unexpected"
			bad++
		}
		fmt.Printf("s3probe %s %s: %s\n", n, fmt.Sprintf(format, a...), w)
	}
	fail := func(what string, err error) int {
		fmt.Printf("s3probe %s %s: %v\n", n, what, err)
		return 1
	}

	small := []byte("small-" + n)
	key := proxy + "/receipts/s" + n
	a, err := do("PUT", key, small, map[string]string{"Content-Type": "text/plain"})
	if err != nil {
		return fail("PUT small", err)
	}
	say(a.status == 200, "PUT s%s (%d bytes) status=%d", n, len(small), a.status)
	a, err = do("HEAD", key, nil, nil)
	if err != nil {
		return fail("HEAD small", err)
	}
	say(a.status == 200 && a.length == int64(len(small)), "HEAD s%s status=%d content-length=%d", n, a.status, a.length)

	data := big(n)
	key = proxy + "/receipts/b" + n
	a, err = do("PUT", key, data, map[string]string{"Content-Type": "application/octet-stream"})
	if err != nil {
		return fail("PUT big", err)
	}
	say(a.status == 200, "PUT b%s (%d bytes) status=%d", n, len(data), a.status)
	a, err = do("HEAD", key, nil, nil)
	if err != nil {
		return fail("HEAD big", err)
	}
	say(a.status == 200 && a.length == bigSize, "HEAD b%s status=%d content-length=%d", n, a.status, a.length)
	a, err = do("GET", key, nil, nil)
	if err != nil {
		return fail("GET big", err)
	}
	say(a.status == 200 && a.length == bigSize && bytes.Equal(a.body, data),
		"GET b%s status=%d content-length=%d body=%d bytes sha256 %s (sent %s)",
		n, a.status, a.length, len(a.body), short(a.body), short(data))

	// If-None-Match: * on a new key, then on the same key
	key = proxy + "/receipts/c" + n
	first := []byte("first-" + n)
	a, err = do("PUT", key, first, map[string]string{"If-None-Match": "*"})
	if err != nil {
		return fail("PUT If-None-Match", err)
	}
	created := a.status == 200
	if a.status == 412 {
		d, err := do("HEAD", direct+"/receipts/c"+n, nil, nil)
		if err != nil {
			return fail("HEAD direct", err)
		}
		say(d.status == 404, "PUT c%s If-None-Match: * on a new key status=412 from the proxy (the server has the key: %t)", n, d.status != 404)
		if bad > 0 {
			return 1
		}
		return 0
	}
	say(created, "PUT c%s If-None-Match: * on a new key status=%d", n, a.status)
	a, err = do("PUT", key, []byte("second-"+n), map[string]string{"If-None-Match": "*"})
	if err != nil {
		return fail("PUT If-None-Match again", err)
	}
	say(a.status == 412, "PUT c%s If-None-Match: * on an existing key status=%d", n, a.status)

	// If-Match with the object's ETag, then with another
	h, err := do("HEAD", key, nil, nil)
	if err != nil {
		return fail("HEAD c", err)
	}
	etag := h.header.Get("ETag")
	say(h.status == 200 && etag != "", "HEAD c%s status=%d etag=%s", n, h.status, etag)
	upd := []byte("updated-" + n)
	a, err = do("PUT", key, upd, map[string]string{"If-Match": etag})
	if err != nil {
		return fail("PUT If-Match", err)
	}
	if a.status == 412 {
		d, err := do("GET", direct+"/receipts/c"+n, nil, nil)
		if err != nil {
			return fail("GET direct", err)
		}
		say(bytes.Equal(d.body, first), "PUT c%s If-Match: its ETag status=412 from the proxy (the server kept %q)", n, d.body)
	} else {
		say(a.status == 200, "PUT c%s If-Match: its ETag status=%d", n, a.status)
	}
	a, err = do("PUT", key, []byte("lost-"+n), map[string]string{"If-Match": `"0123456789abcdef0123456789abcdef"`})
	if err != nil {
		return fail("PUT If-Match other", err)
	}
	say(a.status == 412, "PUT c%s If-Match: another ETag status=%d", n, a.status)
	if bad > 0 {
		return 1
	}
	return 0
}

func check() int {
	f, err := os.Open(os.Getenv("HYDO_ACKED"))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		fmt.Println("cannot read the acknowledged rounds:", err)
		return 2
	}
	k := 0
	if f != nil {
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			n := strings.TrimSpace(sc.Text())
			if n == "" {
				continue
			}
			a, err := do("GET", direct+"/receipts/s"+n, nil, nil)
			if err != nil {
				fmt.Println("s3:", err)
				return 2
			}
			if a.status != 200 || string(a.body) != "small-"+n {
				fmt.Printf("round %s was acknowledged but s%s reads %d %q on the server\n", n, n, a.status, a.body)
				return 1
			}
			a, err = do("GET", direct+"/receipts/b"+n, nil, nil)
			if err != nil {
				fmt.Println("s3:", err)
				return 2
			}
			if a.status != 200 || !bytes.Equal(a.body, big(n)) {
				fmt.Printf("round %s was acknowledged but b%s reads %d with %d bytes on the server\n", n, n, a.status, len(a.body))
				return 1
			}
			k++
		}
	}
	fmt.Printf("%d acknowledged rounds: each object on the server with its bytes\n", k)
	return 0
}

// content is the bytes of a key of hydo-s3-cut.yaml: `size` bytes made from the key's name.
func content(key string, size int) []byte {
	b := make([]byte, size)
	h := sha256.Sum256([]byte(key))
	for i := range b {
		b[i] = h[i%len(h)] ^ byte(i>>5)
	}
	return b
}

// the objects of one round of hydo-s3-cut.yaml: name suffix and size
const (
	smallSize = 64
	largeSize = 1 << 20
	partSize  = 5 << 20 // S3's least size of a part that is not the last
	lastPart  = 1024
)

// event writes an SDK assertion record to the console: the event of a reaction
// (A=s3.acked@<step>+<ms>:cut).
func event(id, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "HYDO-ASSERT {\"t\":\"sometimes\",\"id\":%q,\"ok\":true,\"msg\":%q}\n", id, msg)
}

// ack appends an acknowledged write to $HYDO_ACKED, before the event that a reaction may cut at.
func ack(word string) error {
	f, err := os.OpenFile(os.Getenv("HYDO_ACKED"), os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return err
	}
	if _, err := fmt.Fprintln(f, word); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	event("s3.acked", word)
	return nil
}

// between returns the text between the first `open` and the next `close` in b.
func between(b []byte, open, close string) string {
	s := string(b)
	i := strings.Index(s, open)
	if i < 0 {
		return ""
	}
	s = s[i+len(open):]
	j := strings.Index(s, close)
	if j < 0 {
		return ""
	}
	return s[:j]
}

// cutStep is one round of hydo-s3-cut.yaml through the proxy at $AWS_ENDPOINT_URL_S3. Each write
// that S3 answers 200 (204 for the DELETE) is a word in $HYDO_ACKED and then the event s3.acked:
//
//	put:o<n>a   a PUT of 64 bytes
//	put:o<n>b   a PUT of 1 MiB
//	copy:o<n>c  a copy of o<n>a (x-amz-copy-source)
//	mp:o<n>m    a multipart upload of 5 MiB and 1 KiB, completed
//	del:o<n-1>a a DELETE of the round before's o<n-1>a
func cutStep(n int) int {
	proxy := os.Getenv("AWS_ENDPOINT_URL_S3")
	if proxy == "" {
		fmt.Println("s3probe: AWS_ENDPOINT_URL_S3 is not set")
		return 2
	}
	client.Timeout = 120 * time.Second // a synchronous emulator writes 5 MiB in many flushes
	base := proxy + "/receipts/"
	fail := func(what string, a answer, err error) int {
		if err != nil {
			fmt.Printf("s3probe cut %d %s: %v\n", n, what, err)
		} else {
			fmt.Printf("s3probe cut %d %s: status %d %q\n", n, what, a.status, a.body)
		}
		return 1
	}
	done := func(word string) int {
		if err := ack(word); err != nil {
			fmt.Printf("s3probe cut %d: cannot record %s: %v\n", n, word, err)
			return 2
		}
		fmt.Printf("s3probe cut %d %s acknowledged\n", n, word)
		return 0
	}
	for _, o := range []struct {
		suffix string
		size   int
	}{{"a", smallSize}, {"b", largeSize}} {
		key := fmt.Sprintf("o%d%s", n, o.suffix)
		a, err := do("PUT", base+key, content(key, o.size), nil)
		if err != nil || a.status != 200 {
			return fail("PUT "+key, a, err)
		}
		if r := done("put:" + key); r != 0 {
			return r
		}
	}
	src := fmt.Sprintf("o%da", n)
	key := fmt.Sprintf("o%dc", n)
	a, err := do("PUT", base+key, nil, map[string]string{"x-amz-copy-source": "/receipts/" + src})
	if err != nil || a.status != 200 {
		return fail("copy "+key, a, err)
	}
	if r := done("copy:" + key); r != 0 {
		return r
	}
	key = fmt.Sprintf("o%dm", n)
	a, err = do("POST", base+key+"?uploads", nil, nil)
	if err != nil || a.status != 200 {
		return fail("create multipart "+key, a, err)
	}
	id := between(a.body, "<UploadId>", "</UploadId>")
	if id == "" {
		return fail("create multipart "+key+" (no UploadId)", a, nil)
	}
	whole := content(key, partSize+lastPart)
	var parts strings.Builder
	parts.WriteString("<CompleteMultipartUpload>")
	for i, p := range [][]byte{whole[:partSize], whole[partSize:]} {
		a, err = do("PUT", fmt.Sprintf("%s%s?partNumber=%d&uploadId=%s", base, key, i+1, id), p, nil)
		if err != nil || a.status != 200 {
			return fail(fmt.Sprintf("part %d of %s", i+1, key), a, err)
		}
		fmt.Fprintf(&parts, "<Part><PartNumber>%d</PartNumber><ETag>%s</ETag></Part>", i+1, a.header.Get("ETag"))
	}
	parts.WriteString("</CompleteMultipartUpload>")
	a, err = do("POST", fmt.Sprintf("%s%s?uploadId=%s", base, key, id), []byte(parts.String()), nil)
	if err != nil || a.status != 200 || strings.Contains(string(a.body), "<Error>") {
		return fail("complete multipart "+key, a, err)
	}
	if r := done("mp:" + key); r != 0 {
		return r
	}
	if n > 0 {
		key = fmt.Sprintf("o%da", n-1)
		a, err = do("DELETE", base+key, nil, nil)
		if err != nil || (a.status != 204 && a.status != 200) {
			return fail("DELETE "+key, a, err)
		}
		if r := done("del:" + key); r != 0 {
			return r
		}
	}
	return 0
}

// cutCheck reads every acknowledged write of $HYDO_ACKED from the server's own port: a put, a
// copy and a multipart upload have their bytes, and a deleted key is gone.
func cutCheck() int {
	b, err := os.ReadFile(os.Getenv("HYDO_ACKED"))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		fmt.Println("cannot read the acknowledged writes:", err)
		return 2
	}
	deleted := map[string]bool{}
	var words []string
	for _, w := range strings.Fields(string(b)) {
		words = append(words, w)
		if k, ok := strings.CutPrefix(w, "del:"); ok {
			deleted[k] = true
		}
	}
	lost := 0
	for _, w := range words {
		kind, key, _ := strings.Cut(w, ":")
		a, err := do("GET", direct+"/receipts/"+key, nil, nil)
		if err != nil {
			fmt.Println("s3:", err)
			return 2
		}
		var want []byte
		switch kind {
		case "put":
			size := smallSize
			if strings.HasSuffix(key, "b") {
				size = largeSize
			}
			want = content(key, size)
		case "copy":
			want = content(strings.TrimSuffix(key, "c")+"a", smallSize)
		case "mp":
			want = content(key, partSize+lastPart)
		case "del":
			if a.status != 404 {
				fmt.Printf("lost: %s was acknowledged, but %s reads %d with %d bytes on the server\n", w, key, a.status, len(a.body))
				lost++
			}
			continue
		}
		if deleted[key] {
			continue // a later acknowledged DELETE removed it
		}
		if a.status != 200 || !bytes.Equal(a.body, want) {
			fmt.Printf("lost: %s was acknowledged, but %s reads %d with %d bytes (sha256 %s, sent %s) on the server\n",
				w, key, a.status, len(a.body), short(a.body), short(want))
			lost++
		}
	}
	if lost > 0 {
		fmt.Printf("%d of %d acknowledged writes lost\n", lost, len(words))
		return 1
	}
	fmt.Printf("%d acknowledged writes: each on the server as acknowledged\n", len(words))
	return 0
}

func main() {
	if len(os.Args) == 3 && os.Args[1] == "step" {
		os.Exit(step(os.Args[2]))
	}
	if len(os.Args) == 2 && os.Args[1] == "check" {
		os.Exit(check())
	}
	if len(os.Args) == 3 && os.Args[1] == "cut-step" {
		n, err := strconv.Atoi(os.Args[2])
		if err == nil {
			os.Exit(cutStep(n))
		}
	}
	if len(os.Args) == 2 && os.Args[1] == "cut-check" {
		os.Exit(cutCheck())
	}
	fmt.Fprintln(os.Stderr, "usage: s3probe step <n> | check | cut-step <n> | cut-check")
	os.Exit(2)
}
