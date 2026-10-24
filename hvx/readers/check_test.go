package main

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func TestShellWords(t *testing.T) {
	cases := map[string][]string{
		`a b  c`:                         {"a", "b", "c"},
		`mysql u:@tcp(h:1)/ "SELECT 1"`:  {"mysql", "u:@tcp(h:1)/", "SELECT 1"},
		`x 'it''s' "a\"b" c\ d`:          {"x", "its", `a"b`, "c d"},
		`"" ''`:                          {"", ""},
		"name=file /tmp/a --field line ": {"name=file", "/tmp/a", "--field", "line"},
	}
	for in, want := range cases {
		got, err := shellWords(in)
		if err != nil || !reflect.DeepEqual(got, want) {
			t.Errorf("shellWords(%q) = %q, %v; want %q", in, got, err, want)
		}
	}
	if _, err := shellWords(`"open`); err == nil {
		t.Error("unterminated quote accepted")
	}
}

func TestParseSource(t *testing.T) {
	s, err := parseSource(`acked=hydo acked --add-prefix c`)
	if err != nil || s.name != "acked" || !reflect.DeepEqual(s.args, []string{"hydo", "acked"}) || s.add != "c" {
		t.Fatalf("got %+v %v", s, err)
	}
	s, _ = parseSource(`redis 127.0.0.1:6379 scan "order:*" --strip-prefix order:`)
	if s.name != "redis" || s.strip != "order:" || len(s.args) != 4 {
		t.Fatalf("got %+v", s)
	}
	// a DSN with '=' in it is not a name
	s, _ = parseSource(`postgres "host=x user=y" "SELECT 1"`)
	if s.name != "postgres" {
		t.Fatalf("got name %q", s.name)
	}
}

func TestItemOrder(t *testing.T) {
	x := []string{"10", "9", "b", "a", "100", "-1", "c10", "c4", "c04", "order:2", "c1x", "c1"}
	sortItems(x)
	if strings.Join(x, " ") != "-1 9 10 100 a b c1 c1x c04 c4 c10 order:2" {
		t.Fatal(x)
	}
}

// writeFiles makes a temp dir with the given files; returns the dir.
func writeFiles(t *testing.T, files map[string]string) string {
	d := t.TempDir()
	for n, c := range files {
		if err := os.WriteFile(filepath.Join(d, n), []byte(c), 0644); err != nil {
			t.Fatal(err)
		}
	}
	return d
}

// run executes a check in-process and returns its exit status and last output line.
func run(t *testing.T, args ...string) (int, string) {
	t.Helper()
	r, w, _ := os.Pipe()
	old := os.Stdout
	os.Stdout = w
	rc := checkMain(args)
	w.Close()
	os.Stdout = old
	b := make([]byte, 1<<16)
	n, _ := r.Read(b)
	lines := strings.Split(strings.TrimSpace(string(b[:n])), "\n")
	return rc, lines[len(lines)-1]
}

func TestChecks(t *testing.T) {
	d := writeFiles(t, map[string]string{
		"acked":  "0\n1\n2\n3\n",
		"failed": "5\n7\n",
		"kafka.jsonl": `{"key":"0","offset":0,"partition":0,"value":"100000"}
{"key":"1","offset":1,"partition":0,"value":"100001"}
{"key":"2","offset":2,"partition":0,"value":"100002"}
{"key":"3","offset":3,"partition":0,"value":"100003"}
{"key":"5","offset":4,"partition":0,"value":"100005"}
{"key":"3","offset":5,"partition":0,"value":"100003"}
`,
		"mysql.jsonl": `{"amount":"100000","id":"0"}
{"amount":"100001","id":"1"}
{"amount":"100002","id":"2"}
{"amount":"100003","id":"3"}
{"amount":"999","id":"5"}
`,
		"redis.jsonl": `{"key":"order:1","type":"string","value":"100001"}
{"key":"order:2","type":"string","value":"7"}
{"key":"order:9","type":"string","value":"100009"}
`,
	})
	f := func(n string) string { return filepath.Join(d, n) }
	kafka := "kafka=jsonl " + f("kafka.jsonl")
	acked := "acked=file " + f("acked")
	cases := []struct {
		args []string
		rc   int
		want string
	}{
		{[]string{"subset", "--left", acked, "--right", kafka}, 0, "subset holds: all 4 items of acked are in kafka"},
		{[]string{"subset", "--left", "range 0 6", "--right", kafka}, 1, "subset violated: 1 item of 6 in range not in kafka: 4"},
		{[]string{"exactly-once", "--left", acked, "--right", kafka}, 1, "exactly-once violated: acked 3 is in kafka 2 times (of 4 in acked); duplicated 1: 3 (x2)"},
		{[]string{"exactly-once", "--left", "acked=file " + f("acked") + " --field line", "--right", "jsonl " + f("mysql.jsonl") + " --field id"}, 0, "exactly-once holds: each of the 4 items of acked occurs once in jsonl (jsonl has 5 records)"},
		{[]string{"disjoint", "--left", "told_failed=file " + f("failed"), "--right", kafka}, 1, "disjoint violated: 1 item of told_failed is also in kafka: 5"},
		{[]string{"no-duplicates", "--in", kafka}, 1, "no-duplicates violated: 3 occurs 2 times in kafka; 1 item repeated: 3 (x2)"},
		{[]string{"count", "--in", kafka, "--eq", "6"}, 0, "count holds: kafka has 6 records (want == 6)"},
		{[]string{"count", "--in", kafka, "--max", "2"}, 1, `count violated: kafka has 6 records, want <= 2; first: {"key":"0","offset":0,"partition":0,"value":"100000"}`},
		{[]string{"equal", "--left", acked, "--right", "range 0 4"}, 0, "equal holds: acked and range hold the same 4 items"},
		{[]string{"equal", "--left", acked, "--right", "range 0 5"}, 1, "equal violated: 1 item in range not in acked: 4"},
		{[]string{"never-wrong-value", "--cache", "redis=jsonl " + f("redis.jsonl") + " --strip-prefix order:", "--truth", "mysql=jsonl " + f("mysql.jsonl") + " --key id --value amount"}, 1,
			"never-wrong-value violated: 2 values wrong in redis: 2=7 but mysql has 100002, 9=100009 but mysql has no such key"},
		{[]string{"never-wrong-value", "--allow-orphans", "--cache", "kafka=jsonl " + f("kafka.jsonl"), "--truth", "mysql=jsonl " + f("mysql.jsonl") + " --key id --value amount"}, 1,
			"never-wrong-value violated: 1 value wrong in kafka: 5=100005 but mysql has 999"},
		{[]string{"subset", "--left", "file " + f("nope"), "--right", kafka}, 2, "subset could not be evaluated: could not read file (file): open " + f("nope") + ": no such file or directory"},
		{[]string{"subset", "--unreadable", "violation", "--left", "file " + f("nope"), "--right", kafka}, 1, "subset violated: could not read file (file): open " + f("nope") + ": no such file or directory"},
		{[]string{"subset", "--oracle", "--left", "range 0 6", "--right", kafka}, 1, "HYDO-ORACLE: VIOLATION subset violated: 1 item of 6 in range not in kafka: 4"},
		{[]string{"subset", "--left", "range 0 2"}, 2, ""},
		{[]string{"exactly-once", "--left", "range 0 12 --add-prefix c", "--right", "range 0 4 --add-prefix c"}, 1, "exactly-once violated: range c4 is in range 0 times (of 12 in range); missing 8: c4, c5, c6, c7, c8 (+3 more)"},
		{[]string{"frobnicate", "--left", acked, "--right", kafka}, 2, "frobnicate could not be evaluated: usage: unknown check \"frobnicate\""},
	}
	for _, c := range cases {
		rc, last := run(t, c.args...)
		if rc != c.rc || (c.want != "" && !strings.HasPrefix(last, c.want)) {
			t.Errorf("%q:\n got rc=%d %q\nwant rc=%d %q", c.args, rc, last, c.rc, c.want)
		}
	}
}

func TestAssertRecord(t *testing.T) {
	d := writeFiles(t, map[string]string{"a": "1\n2\n"})
	r, w, _ := os.Pipe()
	old := os.Stdout
	os.Stdout = w
	rc := checkMain([]string{"subset", "--id", "x.y", "--left", "file " + filepath.Join(d, "a"), "--right", "range 0 2"})
	w.Close()
	os.Stdout = old
	b := make([]byte, 4096)
	n, _ := r.Read(b)
	out := string(b[:n])
	want := `HYDO-ASSERT-DECL {"id":"x.y","msg":"subset","t":"always"}
HYDO-ASSERT {"details":{"check":"subset","explanation":"subset violated: 1 item of 2 in file not in range: 2","first":"2"},"id":"x.y","msg":"subset","ok":false,"t":"always"}
subset violated: 1 item of 2 in file not in range: 2
`
	if rc != 1 || out != want {
		t.Fatalf("rc=%d\n%s", rc, out)
	}
}

func TestHydoSource(t *testing.T) {
	d := writeFiles(t, map[string]string{"acked": "1\n3\n", "failed": "2\n3\n", "outcomes": "1 ack\n2 failed\n3 failed\n3 ack\n4 unknown\n5 unknown\n5 ack\n"})
	t.Setenv("HYDO_ACKED", filepath.Join(d, "acked"))
	t.Setenv("HYDO_FAILED", filepath.Join(d, "failed"))
	t.Setenv("HYDO_OUTCOMES", filepath.Join(d, "outcomes"))
	for what, want := range map[string]string{"acked": "1 3", "failed": "2 3", "told-failed": "2", "unknown": "4"} {
		recs, _, err := readStore([]string{"hydo", what})
		if err != nil {
			t.Fatal(err)
		}
		var ids []string
		for _, r := range recs {
			ids = append(ids, r["id"].(string))
		}
		if strings.Join(ids, " ") != want {
			t.Errorf("hydo %s = %v, want %s", what, ids, want)
		}
	}
}
