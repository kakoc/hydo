// The examples of the package documentation (pkg.go.dev) and the test of the module's README:
// every go block of README.md is either the complete program, which runs and must write the
// lines the README shows, or a piece of an example below. This file exists only in the public
// module (sdk/go), not in the frozen copy in hydo/stack.

package hydoassert_test

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/kakoc/hydo/sdk/go/hydoassert"
)

// Declare properties and check them. Declare them as package-level variables in a program, so
// that Catalog lists them.
func Example() {
	stored := hydoassert.Always("kv.stored", "every acknowledged write is stored")
	parses := hydoassert.AlwaysOrUnreachable("kv.log_parses", "every complete log line parses")
	retried := hydoassert.Sometimes("kv.retried", "a client retried a request")
	recovered := hydoassert.Reachable("kv.recovered", "the server replayed a non-empty log")
	unknown := hydoassert.Unreachable("kv.unknown_command", "the server got a command it does not know")

	fmt.Println(stored.Check(true, hydoassert.D{"key": "a"})) // returns the condition
	parses.Check(true, nil)                                   // the details may be nil
	retried.Check(false, nil)                                 // a false Sometimes writes nothing
	recovered.Hit(hydoassert.D{"entries": 3})
	command := "GET"
	if command != "GET" && command != "SET" {
		unknown.Hit(hydoassert.D{"command": command})
	}
	// Output: true
}

// Compare two values: the details get left, op and right.
func ExampleAssertion_Cmp() {
	logSize := hydoassert.Always("kv.log_size", "the log stays under its cap")
	size := int64(12)
	fmt.Println(logSize.Cmp(size, "<=", 10000, nil)) // details: {"left":12,"op":"<=","right":10000}
	// Output: true
}

// Check that something happens in time.
func ExampleEventually() {
	back := hydoassert.Eventually("kv.back", "the server answers within 5 s of its start", 5*time.Second)
	started := time.Now()
	answers := func() bool { return time.Since(started) >= 20*time.Millisecond } // stands for a real request
	fmt.Println(back.Await(answers, 5*time.Millisecond, nil))                    // polls every 5 ms

	// Or mark the start and the end yourself:
	watch := back.Begin()
	// ... the server comes up ...
	watch.Done(nil) // or watch.Expire(nil) when you know it will not
	// Output: true
}

// Add fault points. Without a BUGGIFY seed, Fire returns false and does nothing else.
func ExampleBuggify() {
	lostReply := hydoassert.Buggify("kv.lost_reply_point", "the write lands but its reply is lost")
	slowDisk := hydoassert.Buggify("kv.slow_disk", "a write takes 10 ms longer")

	reply := "OK"
	if lostReply.Fire(nil) {
		reply = "" // misbehave: the client never gets the reply
	}
	if slowDisk.Fire(hydoassert.D{"key": "a"}) {
		time.Sleep(10 * time.Millisecond)
	}
	_ = reply
	fmt.Println("served")
	// Output: served
}

// readmeBlocks returns the info string, text and heading of every fenced block of README.md.
func readmeBlocks(t *testing.T) (info, text, heading []string) {
	b, err := os.ReadFile("../README.md")
	if err != nil {
		t.Fatal(err)
	}
	h, open, cur := "", "", []string(nil)
	inside := false
	for _, line := range strings.Split(string(b), "\n") {
		if !inside && strings.HasPrefix(line, "#") {
			h = strings.TrimSpace(line)
		}
		if strings.HasPrefix(line, "```") {
			if !inside {
				inside, open, cur = true, strings.TrimSpace(line[3:]), nil
			} else {
				inside = false
				info, text, heading = append(info, open), append(text, strings.Join(cur, "\n")+"\n"), append(heading, h)
			}
			continue
		}
		if inside {
			cur = append(cur, line)
		}
	}
	return
}

func goTool(t *testing.T) string {
	if p, err := exec.LookPath("go"); err == nil {
		return p
	}
	return filepath.Join(runtime.GOROOT(), "bin", "go")
}

// TestReadme runs the README's complete program and compares what it writes with the lines the
// README shows after it, and checks that every other go block of the README is a piece of an
// example of this module (so it compiles, and runs with `go test`).
func TestReadme(t *testing.T) {
	info, text, heading := readmeBlocks(t)
	module, err := filepath.Abs("..")
	if err != nil {
		t.Fatal(err)
	}
	var examples strings.Builder
	for _, f := range []string{"example_test.go", "../hydohist/example_test.go"} {
		b, err := os.ReadFile(f)
		if err != nil {
			t.Fatal(err)
		}
		examples.WriteString(strings.ReplaceAll(string(b), "\n\t", "\n"))
	}
	pieces := 0
	for i := range info {
		if info[i] != "go" {
			continue
		}
		if heading[i] == "## A complete program" {
			if i+1 >= len(info) || info[i+1] != "text" {
				t.Fatal("the complete program is not followed by a text block")
			}
			got := runProgram(t, module, text[i])
			if got != text[i+1] {
				t.Errorf("the README's complete program wrote:\n%s\nthe README shows:\n%s", got, text[i+1])
			}
			continue
		}
		pieces++
		if !strings.Contains(examples.String(), text[i]) {
			t.Errorf("README.md (%s): this go block is not a piece of an example in example_test.go:\n%s", heading[i], text[i])
		}
	}
	if pieces < 5 {
		t.Errorf("only %d go blocks besides the complete program", pieces)
	}
	b, _ := os.ReadFile("../README.md")
	for _, part := range strings.Split(string(b), "](")[1:] {
		if !strings.HasPrefix(part, "https://") {
			t.Errorf("README.md links to %q: pkg.go.dev shows the README outside the repository", strings.SplitN(part, ")", 2)[0])
		}
	}
}

// runProgram builds program as the main package of a new module that uses this one, runs it
// and returns what it wrote to HYDO_ASSERT_OUT.
func runProgram(t *testing.T, module, program string) string {
	dir := t.TempDir()
	mod := "module readme\n\ngo 1.22\n\nrequire github.com/kakoc/hydo/sdk/go v0.0.0\n\nreplace github.com/kakoc/hydo/sdk/go => " + module + "\n"
	if err := os.WriteFile(filepath.Join(dir, "go.mod"), []byte(mod), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "main.go"), []byte(program), 0o644); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(dir, "console")
	if err := os.WriteFile(out, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(goTool(t), "run", ".")
	cmd.Dir = dir
	var env []string
	for _, e := range os.Environ() {
		if !strings.HasPrefix(e, "HYDO_") && !strings.HasPrefix(e, "GOFLAGS=") {
			env = append(env, e)
		}
	}
	cmd.Env = append(env, "HYDO_ASSERT_OUT="+out, "HYDO_BUGGIFY_FILE="+filepath.Join(dir, "no-such-file"),
		"GOWORK=off", "GOPROXY=off", "GOFLAGS=-mod=mod", "GOTOOLCHAIN=local")
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		t.Fatalf("go run of the README's program: %v\n%s", err, stderr.String())
	}
	b, err := os.ReadFile(out)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}
